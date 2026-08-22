use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use crate::{
    model::AgentReport,
    private_fs::{self, OwnerPolicy},
};

#[derive(Debug, Clone)]
pub struct Spool {
    directory: PathBuf,
    max_bytes: u64,
    /// Sampling enqueues/evicts while the delivery worker acknowledges files.
    /// The mutex serializes clones in one process; the file lock extends the
    /// same short critical section to another Agent process that opened this
    /// state directory independently.
    mutations: Arc<Mutex<()>>,
    mutation_lock_file: Arc<fs::File>,
}

#[derive(Debug)]
pub struct PendingReport {
    path: PathBuf,
    pub report: AgentReport,
}

impl Spool {
    pub fn open(state_dir: &Path, max_bytes: u64) -> io::Result<Self> {
        let directory = state_dir.join("spool");
        private_fs::ensure_private_directory(&directory)?;
        #[cfg(unix)]
        {
            // Unix permits opening a directory descriptor so the package-created service
            // account ownership can be inherited explicitly. Windows requires directory-
            // specific CreateFile flags; its MSI-managed ACL inheritance already provides the
            // intended ownership boundary, so no directory handle is needed there.
            let directory_handle = fs::File::open(&directory)?;
            private_fs::adopt_parent_owner(&directory_handle, state_dir)?;
        }
        let mutation_lock_file = open_mutation_lock(&directory)?;
        let spool = Self {
            directory,
            max_bytes,
            mutations: Arc::new(Mutex::new(())),
            mutation_lock_file: Arc::new(mutation_lock_file),
        };
        // A process killed between fsync and rename can leave a private atomic
        // temporary behind. Reclaim it and enforce the budget before accepting
        // new reports, including after a restart with no immediate sampling.
        {
            let _guard = spool.mutation_guard()?;
            spool.enforce_limit()?;
        }
        Ok(spool)
    }

    pub fn pending_count(&self) -> io::Result<u64> {
        self.count(JSON)
    }

    /// 只数个数，不收集路径也不排序。
    ///
    /// `pending_count()` 每个采集周期都要调一次。若复用 `paths()`，就要分配一个
    /// `Vec<PathBuf>`、把每个文件名都拷进去、再整体排序，只为了取一个长度——
    /// 积压几千份报文时那是每 10 秒一次的 O(n log n) 加 n 次分配，
    /// 而断线积压恰恰是 Agent 最需要省资源的时候。
    fn count(&self, extension: &str) -> io::Result<u64> {
        let mut total = 0;
        for entry in fs::read_dir(&self.directory)? {
            if entry?
                .path()
                .extension()
                .is_some_and(|value| value == extension)
            {
                total += 1;
            }
        }
        Ok(total)
    }

    pub fn enqueue(&self, report: &AgentReport) -> anyhow::Result<()> {
        let _guard = self.mutation_guard()?;
        let timestamp = report.collected_at.timestamp_millis().max(0);
        let name = format!("{timestamp:020}-{}.json", report.report_id);
        let target = self.directory.join(name);
        let bytes = serde_json::to_vec(report)?;
        private_fs::write_atomic(&target, &bytes, OwnerPolicy::Parent(&self.directory))?;
        self.enforce_limit()?;
        Ok(())
    }

    pub fn oldest(&self) -> anyhow::Result<Option<PendingReport>> {
        let _guard = self.mutation_guard()?;
        // 只求最小值，不必把全部路径排序——文件名以零填充的时间戳开头，
        // 因此字典序即投递顺序，一次线性扫描就能取到队首。
        let Some(path) = self.min_path(JSON)? else {
            return Ok(None);
        };
        let bytes = fs::read(&path)?;
        match serde_json::from_slice(&bytes) {
            Ok(report) => Ok(Some(PendingReport { path, report })),
            Err(error) => {
                let quarantine = path.with_extension(INVALID);
                let _ = fs::rename(&path, quarantine);
                Err(error.into())
            }
        }
    }

    pub fn acknowledge(&self, pending: PendingReport) -> io::Result<()> {
        let _guard = self.mutation_guard()?;
        match fs::remove_file(pending.path) {
            Ok(()) => Ok(()),
            // The sampling task may enforce the bounded spool budget while the
            // delivery worker has the oldest report in memory. Eviction means
            // that report was deliberately discarded from durable storage; a
            // successful send must not be turned into a false disk failure.
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// 队首路径（字典序最小）。O(n) 一趟，不排序、不收集。
    fn min_path(&self, extension: &str) -> io::Result<Option<PathBuf>> {
        let mut smallest: Option<PathBuf> = None;
        for entry in fs::read_dir(&self.directory)? {
            let path = entry?.path();
            if path.extension().is_none_or(|value| value != extension) {
                continue;
            }
            if smallest.as_ref().is_none_or(|current| &path < current) {
                smallest = Some(path);
            }
        }
        Ok(smallest)
    }

    /// 收集并排序全部路径。**只**给容量核算用——它本来就要遍历全部文件算大小，
    /// 排序是为了"先淘汰最老的"。热路径请用 `count()` / `min_path()`。
    fn paths(&self, extension: &str) -> io::Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|value| value == extension) {
                paths.push(path);
            }
        }
        paths.sort();
        Ok(paths)
    }

    /// 把队列总占用压回 `max_bytes` 以内。
    ///
    /// # 隔离文件为什么必须一起算
    ///
    /// 反序列化失败的报文会被改名成 `.invalid` 留作事后排查。容量核算若只统计 `.json`，
    /// 隔离文件就**既不占配额、也永远不被清理**——磁盘异常反复触发时它们会悄悄吃掉
    /// 整个分区，而这恰恰是 Agent 最需要稳健的场景。
    ///
    /// 因此两类文件共用同一份预算，且优先淘汰隔离文件：它们对补传没有价值，只在排查
    /// 时有用，保留最近的少量样本即可。
    fn enforce_limit(&self) -> io::Result<()> {
        private_fs::cleanup_atomic_temporaries(&self.directory)?;
        let quarantined = self
            .paths(INVALID)?
            .into_iter()
            .map(|path| {
                let size = file_size(&path);
                (path, size)
            })
            .collect::<Vec<_>>();
        let pending = self
            .paths(JSON)?
            .into_iter()
            .map(|path| {
                let size = file_size(&path);
                (path, size)
            })
            .collect::<Vec<_>>();
        let mut total = [quarantined.as_slice(), pending.as_slice()]
            .concat()
            .iter()
            .fold(0_u64, |total, (_, size)| total.saturating_add(*size));

        // 淘汰顺序：先隔离文件（最老的先删），再是最老的待发报文。
        for (path, size) in quarantined.iter().chain(pending.iter()) {
            if total <= self.max_bytes {
                break;
            }
            match fs::remove_file(path) {
                Ok(()) => total = total.saturating_sub(*size),
                // An external cleanup may have removed a file after the
                // snapshot. Subtract its snapshotted size, not a fresh zero,
                // so that race cannot evict one additional valid report.
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    total = total.saturating_sub(*size)
                }
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn mutation_guard(&self) -> io::Result<SpoolMutationGuard<'_>> {
        let local = self
            .mutations
            .lock()
            .map_err(|_| io::Error::other("spool filesystem mutation lock was poisoned"))?;
        self.mutation_lock_file.lock()?;
        Ok(SpoolMutationGuard {
            _local: local,
            file: &self.mutation_lock_file,
        })
    }
}

struct SpoolMutationGuard<'a> {
    _local: MutexGuard<'a, ()>,
    file: &'a fs::File,
}

impl Drop for SpoolMutationGuard<'_> {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn open_mutation_lock(directory: &Path) -> io::Result<fs::File> {
    let path = directory.join(".spool.lock");
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    private_fs::adopt_parent_owner(&file, directory)?;
    Ok(file)
}

/// 待发报文；按文件名（时间戳前缀）排序即为投递顺序。
const JSON: &str = "json";
/// 反序列化失败后隔离的报文，仅供排查，不再参与补传。
const INVALID: &str = "invalid";

fn file_size(path: &Path) -> u64 {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;
    use crate::model::*;

    fn report() -> AgentReport {
        AgentReport {
            schema_version: 1,
            report_id: Uuid::new_v4().to_string(),
            collected_at: Utc::now(),
            host: HostIdentity {
                id: Uuid::new_v4().to_string(),
                name: "test".into(),
                os: "test".into(),
                os_version: None,
                kernel_version: None,
                arch: "test".into(),
                agent_version: "test".into(),
            },
            interval_seconds: 10.0,
            system: SystemSnapshot {
                uptime_seconds: 1,
                cpu: CpuSnapshot {
                    usage_percent: 1.0,
                    logical_count: 1,
                    physical_count: Some(1),
                    per_core_percent: vec![1.0],
                },
                memory: MemorySnapshot {
                    total_bytes: 1,
                    used_bytes: 1,
                    available_bytes: 0,
                    swap_total_bytes: 0,
                    swap_used_bytes: 0,
                },
                networks: Vec::new(),
                disks: Vec::new(),
                temperatures: Vec::new(),
                gpus: Vec::new(),
            },
            capabilities: Vec::new(),
            agent: AgentHealth {
                spool_pending_batches: 0,
                collector_errors: 0,
            },
        }
    }

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!("unionc-agent-spool-{}", Uuid::new_v4()))
    }

    #[test]
    fn persists_and_acknowledges_in_order() {
        let path = temp_dir();
        let spool = Spool::open(&path, 1024 * 1024).unwrap();
        spool.enqueue(&report()).unwrap();
        assert_eq!(spool.pending_count().unwrap(), 1);
        let pending = spool.oldest().unwrap().unwrap();
        spool.acknowledge(pending).unwrap();
        assert_eq!(spool.pending_count().unwrap(), 0);
        drop(spool);
        fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn opening_spool_reclaims_abandoned_atomic_writes() {
        let directory = temp_dir();
        let spool_directory = directory.join("spool");
        fs::create_dir_all(&spool_directory).unwrap();
        let abandoned = spool_directory.join(format!(".private-{}.tmp", Uuid::new_v4()));
        fs::write(&abandoned, vec![b'x'; 4096]).unwrap();

        let spool = Spool::open(&directory, 1024 * 1024).unwrap();
        assert!(!abandoned.exists());
        drop(spool);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn independently_opened_spools_share_a_filesystem_mutation_lock() {
        let directory = temp_dir();
        let first = Spool::open(&directory, 1024 * 1024).unwrap();
        let second = Spool::open(&directory, 1024 * 1024).unwrap();

        let first_guard = first.mutation_guard().unwrap();
        assert!(matches!(
            second.mutation_lock_file.try_lock(),
            Err(fs::TryLockError::WouldBlock)
        ));
        drop(first_guard);

        second.mutation_lock_file.try_lock().unwrap();
        second.mutation_lock_file.unlock().unwrap();
        drop(second);
        drop(first);
        fs::remove_dir_all(directory).unwrap();
    }

    /// 损坏的报文会被隔离成 `.invalid`。回归：隔离文件必须计入容量预算，
    /// 否则磁盘异常反复触发时它们会绕开 `max_bytes` 把分区吃满。
    #[test]
    fn quarantined_reports_count_against_the_budget_and_are_evicted_first() {
        let directory = temp_dir();
        let spool = Spool::open(&directory, 4096).unwrap();

        // 造 20 个各 1 KiB 的隔离文件，总量远超 4 KiB 预算。
        for index in 0..20 {
            let path = directory
                .join("spool")
                .join(format!("{index:020}-{}.invalid", Uuid::new_v4()));
            fs::write(&path, vec![b'x'; 1024]).unwrap();
        }
        let quarantined_before = spool.paths(INVALID).unwrap().len();
        assert_eq!(quarantined_before, 20);

        // 一次正常入队会触发容量核算。
        spool.enqueue(&report()).unwrap();

        let quarantined_after = spool.paths(INVALID).unwrap().len();
        assert!(
            quarantined_after < quarantined_before,
            "隔离文件必须参与淘汰，实际仍有 {quarantined_after} 个"
        );

        let total: u64 = [spool.paths(INVALID).unwrap(), spool.paths(JSON).unwrap()]
            .concat()
            .iter()
            .map(|path| file_size(path))
            .sum();
        assert!(
            total <= 4096,
            "spool 总占用应回落到预算内，实际 {total} 字节"
        );

        // 待发报文本身必须活下来——隔离文件先被淘汰。
        assert_eq!(spool.pending_count().unwrap(), 1);

        drop(spool);
        fs::remove_dir_all(directory).unwrap();
    }
}
