use std::{
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};

use anyhow::Context;
use uuid::Uuid;

use crate::{
    model::{AGENT_REPORT_MAX_BODY_BYTES, AgentReport},
    private_fs::{self, OwnerPolicy},
    report_contract,
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
            let path = entry?.path();
            if path.extension().is_some_and(|value| value == extension)
                && fixed_spool_file_metadata(&path)?.is_some()
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
        let (_, bytes) = report_contract::encode_report_body(report)?;
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
        let bytes = read_spool_file_bounded(&path)?;
        if bytes.len() > AGENT_REPORT_MAX_BODY_BYTES {
            let error = anyhow::anyhow!(
                "spool report exceeds the {} byte Agent wire limit",
                AGENT_REPORT_MAX_BODY_BYTES
            );
            let quarantine = path.with_extension(format!("{}.invalid", Uuid::new_v4()));
            fs::rename(&path, &quarantine).with_context(|| {
                format!(
                    "failed to quarantine oversized spool report {} as {}: {error}",
                    path.display(),
                    quarantine.display()
                )
            })?;
            return Err(error);
        }
        match serde_json::from_slice(&bytes) {
            Ok(report) => match report_contract::encode_report_body(&report) {
                Ok((bounded, _)) if bounded == report => Ok(Some(PendingReport { path, report })),
                Ok(_) => {
                    let error = anyhow::anyhow!(
                        "spool report requires changes to satisfy the exact current Agent wire \
                         contract"
                    );
                    let quarantine = path.with_extension(format!("{}.invalid", Uuid::new_v4()));
                    fs::rename(&path, &quarantine).with_context(|| {
                        format!(
                            "failed to quarantine noncanonical spool report {} as {} after \
                             contract error: {error}",
                            path.display(),
                            quarantine.display()
                        )
                    })?;
                    Err(error)
                }
                Err(error) => {
                    let quarantine = path.with_extension(format!("{}.invalid", Uuid::new_v4()));
                    fs::rename(&path, &quarantine).with_context(|| {
                        format!(
                            "failed to quarantine invalid spool report {} as {} after contract error: {error}",
                            path.display(),
                            quarantine.display()
                        )
                    })?;
                    Err(error)
                }
            },
            Err(error) => {
                // Keep every forensic sample and stay portable: Windows rename does not replace
                // an existing destination, so a fixed `.invalid` name can leave the corrupt JSON
                // at the FIFO head forever. The final extension remains `.invalid` so the normal
                // spool budget continues to account for and evict these files.
                let quarantine = path.with_extension(format!("{}.invalid", Uuid::new_v4()));
                fs::rename(&path, &quarantine).with_context(|| {
                    format!(
                        "failed to quarantine invalid spool report {} as {} after JSON error: \
                         {error}",
                        path.display(),
                        quarantine.display()
                    )
                })?;
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
            if fixed_spool_file_metadata(&path)?.is_none() {
                continue;
            }
            if smallest.as_ref().is_none_or(|current| &path < current) {
                smallest = Some(path);
            }
        }
        Ok(smallest)
    }

    /// 测试辅助：收集并排序某类 spool 路径。
    #[cfg(test)]
    fn paths(&self, extension: &str) -> io::Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        for entry in fs::read_dir(&self.directory)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|value| value == extension)
                && fixed_spool_file_metadata(&path)?.is_some()
            {
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
        let mut quarantined = Vec::new();
        let mut pending = Vec::new();
        let mut total = 0_u64;
        let mut entries = 0_u64;
        // One directory scan accounts for both classes. Every file is charged
        // at least one conservative filesystem block; on Unix the actual
        // allocated blocks are also included. This prevents zero/tiny files
        // from bypassing the byte budget through block and inode overhead.
        for entry in fs::read_dir(&self.directory)? {
            let path = entry?.path();
            let target = match path.extension().and_then(|value| value.to_str()) {
                Some(INVALID) => &mut quarantined,
                Some(JSON) => &mut pending,
                _ => continue,
            };
            let Some(metadata) = fixed_spool_file_metadata(&path)? else {
                continue;
            };
            let size = accounted_file_size(&metadata);
            collect_bounded_spool_entry(target, &mut entries, (path, size))?;
            total = total.saturating_add(size);
        }
        quarantined.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        pending.sort_unstable_by(|left, right| left.0.cmp(&right.0));

        // 淘汰顺序：先隔离文件（最老的先删），再是最老的待发报文。
        for (path, size) in quarantined.iter().chain(pending.iter()) {
            if !spool_over_budget(total, entries, self.max_bytes) {
                break;
            }
            match fs::remove_file(path) {
                Ok(()) => {
                    total = total.saturating_sub(*size);
                    entries = entries.saturating_sub(1);
                }
                // An external cleanup may have removed a file after the
                // snapshot. Subtract its snapshotted size, not a fresh zero,
                // so that race cannot evict one additional valid report.
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    total = total.saturating_sub(*size);
                    entries = entries.saturating_sub(1);
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

/// Bound directory/inode overhead even where allocated-block metadata is not
/// exposed by the standard library.
const MIN_ACCOUNTED_FILE_BYTES: u64 = 4 * 1024;
/// A byte setting alone can still permit millions of tiny files when an
/// operator configures a huge budget. Keep scans and inode use absolutely bounded.
const MAX_SPOOL_ENTRIES: u64 = 4_096;
/// Allow one newly enqueued report beyond the durable limit so `enforce_limit`
/// can collect it and evict the oldest entry, while keeping scan memory bounded.
const MAX_SPOOL_SCAN_ENTRIES: u64 = MAX_SPOOL_ENTRIES + 1;

fn spool_over_budget(total_bytes: u64, entries: u64, max_bytes: u64) -> bool {
    total_bytes > max_bytes || entries > MAX_SPOOL_ENTRIES
}

fn collect_bounded_spool_entry<T>(
    target: &mut Vec<T>,
    collected_entries: &mut u64,
    entry: T,
) -> io::Result<()> {
    if *collected_entries >= MAX_SPOOL_SCAN_ENTRIES {
        return Err(io::Error::other(format!(
            "spool scan exceeded the safety limit of {MAX_SPOOL_SCAN_ENTRIES} report files; \
             refusing to collect additional .json/.invalid paths"
        )));
    }
    target.try_reserve(1).map_err(|error| {
        io::Error::other(format!(
            "spool scan could not reserve memory for report file {} within the \
             {MAX_SPOOL_SCAN_ENTRIES}-entry safety limit: {error}",
            collected_entries.saturating_add(1)
        ))
    })?;
    target.push(entry);
    *collected_entries += 1;
    Ok(())
}

fn accounted_file_size(metadata: &fs::Metadata) -> u64 {
    #[cfg(unix)]
    let allocated = {
        use std::os::unix::fs::MetadataExt;
        metadata.blocks().saturating_mul(512)
    };
    #[cfg(not(unix))]
    let allocated = 0;
    metadata.len().max(allocated).max(MIN_ACCOUNTED_FILE_BYTES)
}

/// Return metadata only for a report path that names a fixed regular file.
/// `symlink_metadata` deliberately inspects the directory entry itself so a
/// Unix symlink cannot escape the private spool. Windows file symlinks and
/// other name-surrogate objects are reparse points, which must be rejected
/// explicitly even if their metadata otherwise resembles a file.
fn fixed_spool_file_metadata(path: &Path) -> io::Result<Option<fs::Metadata>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_file() {
        return Ok(None);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        if !windows_file_attributes_are_fixed(metadata.file_attributes()) {
            return Ok(None);
        }
    }
    Ok(Some(metadata))
}

#[cfg(windows)]
fn windows_file_attributes_are_fixed(attributes: u32) -> bool {
    use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

    attributes & FILE_ATTRIBUTE_REPARSE_POINT.0 == 0
}

#[cfg(test)]
fn file_size(path: &Path) -> io::Result<u64> {
    fixed_spool_file_metadata(path)?
        .map(|metadata| accounted_file_size(&metadata))
        .ok_or_else(|| io::Error::other("spool candidate is not a fixed regular file"))
}

fn read_spool_file_bounded(path: &Path) -> io::Result<Vec<u8>> {
    let file = fs::File::open(path)?;
    let read_limit = u64::try_from(AGENT_REPORT_MAX_BODY_BYTES)
        .expect("Agent report limit fits u64")
        .saturating_add(1);
    let mut bytes = Vec::new();
    file.take(read_limit).read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;
    use crate::model::*;

    fn report() -> AgentReport {
        AgentReport {
            schema_version: 1,
            report_id: Uuid::new_v4().to_string(),
            collected_at: Utc::now(),
            host: HostIdentity {
                id: Uuid::new_v4().to_string(),
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
    fn a_json_named_directory_is_not_a_spool_candidate() {
        let directory = temp_dir();
        let spool = Spool::open(&directory, 1024 * 1024).unwrap();
        let fake_head = directory
            .join("spool")
            .join(format!("{zero:020}-directory.json", zero = 0));
        fs::create_dir(&fake_head).unwrap();

        let valid = report();
        spool.enqueue(&valid).unwrap();

        assert_eq!(spool.pending_count().unwrap(), 1);
        let pending = spool
            .oldest()
            .unwrap()
            .expect("the regular report remains the FIFO head");
        assert_eq!(pending.report.report_id, valid.report_id);

        drop(spool);
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_json_symlink_is_not_a_spool_candidate() {
        use std::os::unix::fs::symlink;

        let directory = temp_dir();
        let spool = Spool::open(&directory, 1024 * 1024).unwrap();
        let linked_report = report();
        let (_, linked_bytes) = report_contract::encode_report_body(&linked_report).unwrap();
        let outside = directory.join("outside.json");
        fs::write(&outside, linked_bytes).unwrap();
        let link = directory
            .join("spool")
            .join(format!("{zero:020}-symlink.json", zero = 0));
        symlink(&outside, &link).unwrap();

        assert_eq!(spool.pending_count().unwrap(), 0);
        assert!(spool.oldest().unwrap().is_none());
        spool.enforce_limit().unwrap();
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );

        drop(spool);
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_reparse_attributes_are_not_fixed() {
        use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

        assert!(!windows_file_attributes_are_fixed(
            FILE_ATTRIBUTE_REPARSE_POINT.0
        ));
        assert!(windows_file_attributes_are_fixed(0));
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

        // 造 20 个各 1 KiB 的隔离文件；每个至少按一个 4 KiB 块记账。
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
            .map(|path| file_size(path).unwrap())
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

    #[test]
    fn tiny_files_and_huge_configurations_still_have_resource_bounds() {
        let directory = temp_dir();
        fs::create_dir_all(&directory).unwrap();
        let empty = directory.join("empty");
        fs::write(&empty, []).unwrap();
        let metadata = fs::metadata(&empty).unwrap();
        assert_eq!(accounted_file_size(&metadata), MIN_ACCOUNTED_FILE_BYTES);

        assert!(!spool_over_budget(0, MAX_SPOOL_ENTRIES, u64::MAX));
        assert!(spool_over_budget(0, MAX_SPOOL_ENTRIES + 1, u64::MAX));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn spool_scan_stops_collecting_at_the_entry_limit() {
        let mut quarantined = Vec::new();
        let mut pending = Vec::new();
        let mut entries = 0_u64;

        for index in 0..MAX_SPOOL_SCAN_ENTRIES {
            let target = if index % 2 == 0 {
                &mut quarantined
            } else {
                &mut pending
            };
            collect_bounded_spool_entry(target, &mut entries, index).unwrap();
        }

        let error = collect_bounded_spool_entry(&mut pending, &mut entries, MAX_SPOOL_SCAN_ENTRIES)
            .expect_err("the first over-limit path must be rejected before collection");
        assert!(error.to_string().contains("safety limit of 4097"));
        assert_eq!(entries, MAX_SPOOL_SCAN_ENTRIES);
        assert_eq!(
            quarantined.len() + pending.len(),
            usize::try_from(MAX_SPOOL_SCAN_ENTRIES).unwrap(),
            "the rejected path must not grow either scan vector"
        );
    }

    #[test]
    fn an_existing_fixed_quarantine_name_cannot_pin_a_corrupt_fifo_head() {
        let directory = temp_dir();
        let spool = Spool::open(&directory, 1024 * 1024).unwrap();
        let corrupt =
            directory
                .join("spool")
                .join(format!("{zero:020}-{}.json", Uuid::new_v4(), zero = 0));
        let old_fixed_quarantine = corrupt.with_extension(INVALID);
        fs::write(&corrupt, b"not valid JSON").unwrap();
        fs::write(&old_fixed_quarantine, b"previous forensic sample").unwrap();

        let valid = report();
        spool.enqueue(&valid).unwrap();
        let error = spool
            .oldest()
            .expect_err("the corrupt head is reported after it is quarantined");
        assert!(error.to_string().contains("expected ident"));
        assert!(!corrupt.exists(), "the corrupt JSON must leave the FIFO");
        assert!(
            old_fixed_quarantine.exists(),
            "existing forensic data is preserved"
        );
        assert_eq!(
            spool.paths(INVALID).unwrap().len(),
            2,
            "the new unique quarantine still uses the budgeted .invalid extension"
        );

        let pending = spool
            .oldest()
            .unwrap()
            .expect("the next valid report remains deliverable");
        assert_eq!(pending.report.report_id, valid.report_id);

        drop(spool);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn an_oversized_spool_report_is_bounded_and_quarantined_before_json_parsing() {
        let directory = temp_dir();
        let spool = Spool::open(&directory, 16 * 1024 * 1024).unwrap();
        let mut noncanonical = report();
        let long_mount = format!("/{}", "界".repeat(1300));
        noncanonical.system.disks = (0..AGENT_REPORT_MAX_DISKS)
            .map(|index| DiskSnapshot {
                name: format!("disk-{index:04}"),
                mount_point: format!("{long_mount}-{index}"),
                file_system: "ext4".into(),
                total_bytes: 100,
                available_bytes: 50,
                read_bytes_total: 1,
                written_bytes_total: 1,
                read_bytes_per_second: 1.0,
                written_bytes_per_second: 1.0,
                is_read_only: false,
            })
            .collect();
        let bytes = serde_json::to_vec(&noncanonical).unwrap();
        assert!(bytes.len() > AGENT_REPORT_MAX_BODY_BYTES);
        let path = directory.join("spool").join(format!(
            "{zero:020}-{}.json",
            noncanonical.report_id,
            zero = 0
        ));
        fs::write(&path, bytes).unwrap();

        let error = spool
            .oldest()
            .expect_err("oversized reports must fail closed");
        assert!(
            error
                .to_string()
                .contains("exceeds the 524288 byte Agent wire limit")
        );
        assert!(!path.exists());
        assert_eq!(spool.paths(INVALID).unwrap().len(), 1);

        drop(spool);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn an_unknown_spool_schema_is_quarantined_instead_of_rewritten() {
        let directory = temp_dir();
        let spool = Spool::open(&directory, 1024 * 1024).unwrap();
        let mut future = report();
        future.schema_version = AGENT_REPORT_SCHEMA_VERSION + 1;
        let path =
            directory
                .join("spool")
                .join(format!("{zero:020}-{}.json", future.report_id, zero = 0));
        fs::write(&path, serde_json::to_vec(&future).unwrap()).unwrap();

        let error = spool.oldest().expect_err("unknown schemas fail closed");
        assert!(
            error
                .to_string()
                .contains("unsupported Agent report schema_version")
        );
        assert!(!path.exists());
        assert_eq!(spool.paths(INVALID).unwrap().len(), 1);

        drop(spool);
        fs::remove_dir_all(directory).unwrap();
    }
}
