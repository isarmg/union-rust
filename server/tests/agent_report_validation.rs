//! `AgentReport::validate()` 的分支覆盖。
//!
//! 这个函数是 Agent 数据进入系统的**唯一安全边界**：它之后的代码（摘要计算、
//! 数据库写入、前端渲染）都假定报文已经过校验。仅有一个测试的覆盖是不够的，
//! 且测的是 `metric_summary()` 而非 `validate()`。
//!
//! 组织方式：一份合法基准报文 + 逐个字段施加"应被拒绝"的变异。同时保留一组
//! **边界值必须被接受**的用例——只测拒绝路径的话，把校验改得过严也不会有测试失败。

use chrono::{Duration, Utc};
use serde_json::{Value, json};
use unionc::monitoring::{
    AGENT_REPORT_MAX_CAPABILITIES, AGENT_REPORT_MAX_CPU_CORES, AGENT_REPORT_MAX_DISKS,
    AGENT_REPORT_MAX_GPUS, AGENT_REPORT_MAX_NETWORKS, AGENT_REPORT_MAX_TEMPERATURES, AgentHealth,
    AgentReport, AgentReportExt, CpuSnapshot, HostIdentity, MemorySnapshot, SystemSnapshot,
};
use uuid::Uuid;

// ─── 基准报文 ────────────────────────────────────────────────────────────────

fn valid_report() -> Value {
    json!({
        "schema_version": 1,
        "report_id": Uuid::new_v4(),
        "collected_at": Utc::now(),
        "host": {
            "id": Uuid::new_v4(),
            "name": "valid-host",
            "os": "linux",
            "os_version": "6.1.0",
            "kernel_version": "6.1.0",
            "arch": "x86_64",
            "agent_version": "0.3.2"
        },
        "interval_seconds": 10.0,
        "system": {
            "uptime_seconds": 3600,
            "cpu": {
                "usage_percent": 25.0, "logical_count": 4,
                "physical_count": 2, "per_core_percent": [10.0, 20.0, 30.0, 40.0]
            },
            "memory": {
                "total_bytes": 1000, "used_bytes": 400, "available_bytes": 600,
                "swap_total_bytes": 100, "swap_used_bytes": 50
            },
            "networks": [ network("eth0") ],
            "disks": [ disk("sda") ],
            "temperatures": [ temperature(55.0) ],
            "gpus": [ gpu() ]
        },
        "capabilities": [{
            "name": "system.cpu", "available": true,
            "source": "sysinfo", "error_kind": null, "message": null
        }],
        "agent": { "spool_pending_batches": 0, "collector_errors": 0 }
    })
}

fn network(name: &str) -> Value {
    json!({
        "name": name,
        "received_bytes_total": 1000, "transmitted_bytes_total": 2000,
        "received_bytes_per_second": 100.0, "transmitted_bytes_per_second": 50.0,
        "packets_received_total": 10, "packets_transmitted_total": 20,
        "receive_errors_total": 0, "transmit_errors_total": 0
    })
}

fn disk(name: &str) -> Value {
    json!({
        "name": name, "mount_point": "/", "file_system": "ext4",
        "total_bytes": 1000, "available_bytes": 500,
        "read_bytes_total": 10, "written_bytes_total": 20,
        "read_bytes_per_second": 5.0, "written_bytes_per_second": 6.0,
        "is_read_only": false
    })
}

fn temperature(celsius: f64) -> Value {
    json!({
        "id": "cpu-0", "label": "CPU", "celsius": celsius,
        "max_celsius": null, "critical_celsius": 100.0, "source": "hwmon"
    })
}

fn gpu() -> Value {
    json!({
        "id": "gpu-0", "vendor": "nvidia", "name": "Test GPU",
        "utilization_percent": 50.0,
        "memory_total_bytes": 8000, "memory_used_bytes": 2000,
        "temperature_celsius": 70.0, "power_watts": 150.0,
        "core_clock_mhz": 1500.0, "memory_clock_mhz": 7000.0,
        "pcie_rx_bytes_per_second": 1000.0, "pcie_tx_bytes_per_second": 2000.0,
        "source": "nvml"
    })
}

/// 按 JSON Pointer 路径替换一个值。
fn patch(mut report: Value, pointer: &str, value: Value) -> Value {
    *report
        .pointer_mut(pointer)
        .unwrap_or_else(|| panic!("基准报文中不存在路径 {pointer}——测试与结构已失同步")) = value;
    report
}

/// 断言报文被拒绝：要么反序列化失败（类型层面就挡住），要么 `validate()` 返回错误。
fn assert_rejected(case: &str, report: Value) {
    match serde_json::from_value::<AgentReport>(report) {
        Err(_) => {} // 类型层面拒绝，同样算守住了边界
        Ok(parsed) => assert!(
            parsed.validate().is_err(),
            "[{case}] 报文通过了 validate()，但它本应被拒绝"
        ),
    }
}

fn assert_accepted(case: &str, report: Value) {
    let parsed = serde_json::from_value::<AgentReport>(report)
        .unwrap_or_else(|error| panic!("[{case}] 反序列化失败：{error}"));
    if let Err(error) = parsed.validate() {
        panic!("[{case}] 合法报文被 validate() 拒绝：{error}");
    }
}

// ─── 基准自检 ────────────────────────────────────────────────────────────────

#[test]
fn baseline_report_is_valid() {
    // 若基准本身不合法，下面所有"变异后被拒绝"的断言都会变成假阳性。
    assert_accepted("基准报文", valid_report());
}

// ─── 拒绝路径 ────────────────────────────────────────────────────────────────

#[test]
fn rejects_invalid_identity_fields() {
    let long_name = "n".repeat(256);
    let long_os = "o".repeat(65);
    let long_arch = "a".repeat(65);
    let long_version = "v".repeat(129);

    for (case, pointer, value) in [
        ("host.id 非 UUID", "/host/id", json!("not-a-uuid")),
        ("host.id 为空", "/host/id", json!("")),
        ("host.name 为空", "/host/name", json!("")),
        ("host.name 仅空白", "/host/name", json!("   ")),
        ("host.name 超长", "/host/name", json!(long_name)),
        ("host.name 含控制字符", "/host/name", json!("host\u{0}name")),
        ("host.name 含换行", "/host/name", json!("host\nname")),
        ("host.os 为空", "/host/os", json!("")),
        ("host.os 超长", "/host/os", json!(long_os)),
        ("host.arch 为空", "/host/arch", json!("")),
        ("host.arch 超长", "/host/arch", json!(long_arch)),
        ("host.agent_version 为空", "/host/agent_version", json!("")),
        (
            "host.agent_version 过旧",
            "/host/agent_version",
            json!("0.3.1"),
        ),
        (
            "host.agent_version 超长",
            "/host/agent_version",
            json!(long_version),
        ),
        ("report_id 非 UUID", "/report_id", json!("nope")),
    ] {
        assert_rejected(case, patch(valid_report(), pointer, value));
    }
}

#[test]
fn rejects_unsupported_schema_version() {
    for version in [0, 2, 65535_u64] {
        assert_rejected(
            &format!("schema_version={version}"),
            patch(valid_report(), "/schema_version", json!(version)),
        );
    }
}

#[test]
fn rejects_interval_outside_the_contract_range() {
    for value in [0.0, 0.09, 3600.01, 86400.0, -1.0] {
        assert_rejected(
            &format!("interval_seconds={value}"),
            patch(valid_report(), "/interval_seconds", json!(value)),
        );
    }
}

#[test]
fn rejects_timestamps_too_far_in_the_future() {
    // 允许少量时钟偏移（5 分钟），但不能接受任意未来时间——否则伪造的报文会
    // 永远排在历史曲线最右端，并让 latest_collected_at 卡在未来。
    for minutes in [6, 60, 60 * 24] {
        assert_rejected(
            &format!("collected_at 超前 {minutes} 分钟"),
            patch(
                valid_report(),
                "/collected_at",
                json!(Utc::now() + Duration::minutes(minutes)),
            ),
        );
    }
}

#[test]
fn rejects_reports_with_too_many_devices() {
    for (case, pointer, count, item) in [
        (
            "networks 超过共享上限",
            "/system/networks",
            AGENT_REPORT_MAX_NETWORKS + 1,
            network("eth"),
        ),
        (
            "disks 超过共享上限",
            "/system/disks",
            AGENT_REPORT_MAX_DISKS + 1,
            disk("sd"),
        ),
        (
            "temperatures 超过共享上限",
            "/system/temperatures",
            AGENT_REPORT_MAX_TEMPERATURES + 1,
            temperature(50.0),
        ),
        (
            "gpus 超过共享上限",
            "/system/gpus",
            AGENT_REPORT_MAX_GPUS + 1,
            gpu(),
        ),
        // per_core_percent 曾是这份清单里唯一的缺口。它不含文本，因此逃过了逐字段的
        // 文本长度校验；又不在设备计数里，于是 512 KiB 的 body 之内可以塞进约 10 万个
        // 浮点数——它们会完整落进 payload JSON 文本，并由详情接口原样回传给控制台。
        (
            "per_core_percent 超过共享上限",
            "/system/cpu/per_core_percent",
            AGENT_REPORT_MAX_CPU_CORES + 1,
            json!(1.0),
        ),
    ] {
        let items = Value::Array(vec![item; count]);
        assert_rejected(case, patch(valid_report(), pointer, items));
    }

    // 上限之内必须仍然接受：4096 核对任何现实硬件都够用，不能把合法主机挡在门外。
    assert_accepted(
        "per_core_percent 恰好 4096",
        patch(
            patch(
                valid_report(),
                "/system/cpu/logical_count",
                json!(AGENT_REPORT_MAX_CPU_CORES),
            ),
            "/system/cpu/per_core_percent",
            Value::Array(vec![json!(1.0); AGENT_REPORT_MAX_CPU_CORES]),
        ),
    );

    let capabilities = Value::Array(vec![
        json!({ "name": "x", "available": true, "source": "s",
                "error_kind": null, "message": null });
        AGENT_REPORT_MAX_CAPABILITIES + 1
    ]);
    assert_rejected(
        "capabilities 超过 256",
        patch(valid_report(), "/capabilities", capabilities),
    );
}

/// capability 的文本字段若**只受数量约束**，内容长度就完全不限。
///
/// 这些字段会整体存进 `monitored_hosts.capabilities` 并原样回传给控制台，因此一台被
/// 攻陷的 Agent 可以在数量合法（≤256）的前提下，每次上报塞进接近 body 上限的任意文本。
/// 其余每个文本字段都走 `validate_text`，这里曾是唯一的缺口。
#[test]
fn rejects_oversized_capability_text_fields() {
    let capability = |field: &str, value: Value| {
        let mut entry = json!({
            "name": "system.cpu", "available": true,
            "source": "sysinfo", "error_kind": null, "message": null
        });
        entry[field] = value;
        patch(valid_report(), "/capabilities", json!([entry]))
    };

    for (case, field, value) in [
        ("name 超长", "name", json!("n".repeat(129))),
        ("source 超长", "source", json!("s".repeat(129))),
        ("error_kind 超长", "error_kind", json!("e".repeat(129))),
        ("message 超长", "message", json!("m".repeat(1025))),
        // 空白与控制字符同样必须被挡住，与其他文本字段一致。
        ("name 为空", "name", json!("   ")),
        ("source 含控制字符", "source", json!("sys\u{0}info")),
    ] {
        assert_rejected(case, capability(field, value));
    }

    // 边界内的值必须仍被接受——否则合法 Agent 会被误伤。
    for (case, field, value) in [
        ("name 恰好 128", "name", json!("n".repeat(128))),
        ("source 恰好 128", "source", json!("s".repeat(128))),
        ("message 恰好 1024", "message", json!("m".repeat(1024))),
    ] {
        assert_accepted(case, capability(field, value));
    }
}

/// 报文里**每一个**文本字段都必须限长。
///
/// 同类问题在 GPU、温度、磁盘与主机版本号上原样
/// 存在——8 个字段完全不受约束，而当时的代码注释却写着"其余每一个文本字段都走
/// validate_text"。这个用例的价值不在于某一个字段，而在于**穷尽**：新增文本字段
/// 时若忘了加校验，这里会失败。
#[test]
fn rejects_oversized_text_in_every_reported_string_field() {
    let huge = "A".repeat(100_000);

    for (case, pointer) in [
        ("host.os_version", "/host/os_version"),
        ("host.kernel_version", "/host/kernel_version"),
        ("disk.file_system", "/system/disks/0/file_system"),
        ("temperature.id", "/system/temperatures/0/id"),
        ("temperature.label", "/system/temperatures/0/label"),
        ("temperature.source", "/system/temperatures/0/source"),
        ("gpu.id", "/system/gpus/0/id"),
        ("gpu.vendor", "/system/gpus/0/vendor"),
        ("gpu.name", "/system/gpus/0/name"),
        ("gpu.source", "/system/gpus/0/source"),
    ] {
        assert_rejected(case, patch(valid_report(), pointer, json!(huge)));
    }
}

/// 控制字符同样要挡住：这些文本会落库并原样回传给控制台。
#[test]
fn rejects_control_characters_in_reported_string_fields() {
    let sneaky = "label\u{0}\u{7}injected";

    for (case, pointer) in [
        ("host.os_version", "/host/os_version"),
        ("disk.file_system", "/system/disks/0/file_system"),
        ("temperature.label", "/system/temperatures/0/label"),
        ("gpu.name", "/system/gpus/0/name"),
    ] {
        assert_rejected(case, patch(valid_report(), pointer, json!(sneaky)));
    }
}

/// 与上面成对：**空字符串必须继续被接受**。
///
/// 采集侧确实会产出空串——Windows 无卷标卷的 `disk.name`、伪文件系统的
/// `file_system`、无标签传感器的 `label`
/// （`temperature.id` 还会回退到这个空 label）、取不到版本号时的 `os_version`。
/// 若把这些字段一并要求非空，代价是一份完全正常的报文被整体拒绝，
/// 即用可用性换一个并不存在的安全收益。限长与禁控制字符才是真正要守的边界。
#[test]
fn still_accepts_empty_optional_text_that_collectors_really_produce() {
    for (case, pointer) in [
        ("disk.name 为空（Windows 无卷标卷）", "/system/disks/0/name"),
        (
            "disk.file_system 为空（伪文件系统）",
            "/system/disks/0/file_system",
        ),
        (
            "temperature.label 为空（无标签传感器）",
            "/system/temperatures/0/label",
        ),
        (
            "temperature.id 为空（回退到空 label）",
            "/system/temperatures/0/id",
        ),
        ("gpu.vendor 为空", "/system/gpus/0/vendor"),
    ] {
        assert_accepted(case, patch(valid_report(), pointer, json!("")));
    }

    // 版本号取不到时是 null，不是空串——两种缺失形态都要放行。
    for (case, pointer) in [
        ("host.os_version 为 null", "/host/os_version"),
        ("host.kernel_version 为 null", "/host/kernel_version"),
    ] {
        assert_accepted(case, patch(valid_report(), pointer, json!(null)));
    }
}

#[test]
fn rejects_impossible_cpu_values() {
    for (case, pointer, value) in [
        (
            "cpu.usage_percent 为负",
            "/system/cpu/usage_percent",
            json!(-0.1),
        ),
        (
            "cpu.usage_percent 超过 100",
            "/system/cpu/usage_percent",
            json!(100.1),
        ),
        (
            "cpu.logical_count 为 0",
            "/system/cpu/logical_count",
            json!(0),
        ),
        (
            "per_core_percent 少于 logical_count",
            "/system/cpu/per_core_percent",
            json!([10.0, 20.0, 30.0]),
        ),
        (
            "per_core_percent 多于 logical_count",
            "/system/cpu/per_core_percent",
            json!([10.0, 20.0, 30.0, 40.0, 50.0]),
        ),
        (
            "cpu.physical_count 为 0",
            "/system/cpu/physical_count",
            json!(0),
        ),
        (
            "cpu.physical_count 大于 logical_count",
            "/system/cpu/physical_count",
            json!(5),
        ),
        (
            "per_core_percent 含负值",
            "/system/cpu/per_core_percent",
            json!([10.0, -5.0, 30.0, 40.0]),
        ),
        (
            "per_core_percent 含超限值",
            "/system/cpu/per_core_percent",
            json!([10.0, 101.0, 30.0, 40.0]),
        ),
    ] {
        assert_rejected(case, patch(valid_report(), pointer, value));
    }
}

#[test]
fn rejects_memory_counters_exceeding_totals() {
    for (case, pointer, value) in [
        ("used > total", "/system/memory/used_bytes", json!(1001)),
        (
            "available > total",
            "/system/memory/available_bytes",
            json!(1001),
        ),
        (
            "swap_used > swap_total",
            "/system/memory/swap_used_bytes",
            json!(101),
        ),
    ] {
        assert_rejected(case, patch(valid_report(), pointer, value));
    }
}

#[test]
fn rejects_invalid_network_entries() {
    for (case, pointer, value) in [
        ("network.name 为空", "/system/networks/0/name", json!("")),
        (
            "network.name 含控制字符",
            "/system/networks/0/name",
            json!("eth\u{7}0"),
        ),
        (
            "接收速率为负",
            "/system/networks/0/received_bytes_per_second",
            json!(-1.0),
        ),
        (
            "发送速率为负",
            "/system/networks/0/transmitted_bytes_per_second",
            json!(-1.0),
        ),
    ] {
        assert_rejected(case, patch(valid_report(), pointer, value));
    }
}

#[test]
fn rejects_invalid_disk_entries() {
    for (case, pointer, value) in [
        (
            "disk.name 含控制字符",
            "/system/disks/0/name",
            json!("data\u{7}"),
        ),
        ("mount_point 为空", "/system/disks/0/mount_point", json!("")),
        (
            "mount_point 含控制字符",
            "/system/disks/0/mount_point",
            json!("/mnt\u{0}/x"),
        ),
        (
            "可用空间大于总空间",
            "/system/disks/0/available_bytes",
            json!(1001),
        ),
        (
            "读速率为负",
            "/system/disks/0/read_bytes_per_second",
            json!(-1.0),
        ),
        (
            "写速率为负",
            "/system/disks/0/written_bytes_per_second",
            json!(-1.0),
        ),
    ] {
        assert_rejected(case, patch(valid_report(), pointer, value));
    }
}

#[test]
fn rejects_temperatures_outside_physical_range() {
    // 低于绝对零度或高于 1000°C 都不是真实读数，多半是单位错误或传感器故障。
    for field in ["celsius", "max_celsius", "critical_celsius"] {
        for value in [-273.16, -1000.0, 1000.1, 1e6] {
            assert_rejected(
                &format!("temperature.{field}={value}"),
                patch(
                    valid_report(),
                    &format!("/system/temperatures/0/{field}"),
                    json!(value),
                ),
            );
        }
    }
}

#[test]
fn rejects_invalid_gpu_entries() {
    for (case, pointer, value) in [
        (
            "利用率为负",
            "/system/gpus/0/utilization_percent",
            json!(-1.0),
        ),
        (
            "利用率超过 100",
            "/system/gpus/0/utilization_percent",
            json!(100.1),
        ),
        (
            "已用显存超过总显存",
            "/system/gpus/0/memory_used_bytes",
            json!(8001),
        ),
        (
            "温度低于绝对零度",
            "/system/gpus/0/temperature_celsius",
            json!(-273.16),
        ),
        (
            "温度过高",
            "/system/gpus/0/temperature_celsius",
            json!(1000.1),
        ),
        ("功率为负", "/system/gpus/0/power_watts", json!(-1.0)),
        ("核心频率为负", "/system/gpus/0/core_clock_mhz", json!(-1.0)),
        (
            "显存频率为负",
            "/system/gpus/0/memory_clock_mhz",
            json!(-1.0),
        ),
        (
            "PCIe 接收速率为负",
            "/system/gpus/0/pcie_rx_bytes_per_second",
            json!(-1.0),
        ),
        (
            "PCIe 发送速率为负",
            "/system/gpus/0/pcie_tx_bytes_per_second",
            json!(-1.0),
        ),
    ] {
        assert_rejected(case, patch(valid_report(), pointer, value));
    }
}

// ─── 边界值必须被接受 ────────────────────────────────────────────────────────

#[test]
fn accepts_values_exactly_on_the_boundary() {
    for (case, pointer, value) in [
        ("interval 恰为下限 0.1", "/interval_seconds", json!(0.1)),
        ("interval 恰为上限 3600", "/interval_seconds", json!(3600.0)),
        ("cpu 使用率恰为 0", "/system/cpu/usage_percent", json!(0.0)),
        (
            "cpu 使用率恰为 100",
            "/system/cpu/usage_percent",
            json!(100.0),
        ),
        (
            "物理核数恰等于逻辑核数",
            "/system/cpu/physical_count",
            json!(4),
        ),
        (
            "已用内存恰等于总量",
            "/system/memory/used_bytes",
            json!(1000),
        ),
        (
            "可用磁盘恰等于总量",
            "/system/disks/0/available_bytes",
            json!(1000),
        ),
        (
            "温度恰为绝对零度",
            "/system/temperatures/0/celsius",
            json!(-273.15),
        ),
        (
            "温度恰为上限 1000",
            "/system/temperatures/0/celsius",
            json!(1000.0),
        ),
        (
            "显存已用恰等于总量",
            "/system/gpus/0/memory_used_bytes",
            json!(8000),
        ),
        (
            "速率恰为 0",
            "/system/networks/0/received_bytes_per_second",
            json!(0.0),
        ),
    ] {
        assert_accepted(case, patch(valid_report(), pointer, value));
    }
}

#[test]
fn accepts_clock_skew_within_the_allowed_window() {
    assert_accepted(
        "collected_at 超前 4 分钟（时钟偏移容忍范围内）",
        patch(
            valid_report(),
            "/collected_at",
            json!(Utc::now() + Duration::minutes(4)),
        ),
    );
}

#[test]
fn accepts_device_counts_exactly_at_the_limit() {
    assert_accepted(
        "networks 恰为 1024",
        patch(
            valid_report(),
            "/system/networks",
            Value::Array(vec![network("eth"); 1024]),
        ),
    );
    assert_accepted(
        "gpus 恰为 128",
        patch(
            valid_report(),
            "/system/gpus",
            Value::Array(vec![gpu(); 128]),
        ),
    );
}

#[test]
fn accepts_empty_device_collections() {
    // 无网卡/无磁盘/无传感器/无 GPU 都是合法状态（容器、无独显主机等）。
    let mut report = valid_report();
    for pointer in [
        "/system/networks",
        "/system/disks",
        "/system/temperatures",
        "/system/gpus",
    ] {
        report = patch(report, pointer, json!([]));
    }
    assert_accepted("全部设备集合为空", report);
}

#[test]
fn accepts_absent_optional_fields() {
    let mut report = valid_report();
    for pointer in [
        "/host/os_version",
        "/host/kernel_version",
        "/system/cpu/physical_count",
        "/system/gpus/0/utilization_percent",
        "/system/gpus/0/memory_total_bytes",
        "/system/gpus/0/memory_used_bytes",
        "/system/gpus/0/temperature_celsius",
        "/system/gpus/0/power_watts",
        "/system/temperatures/0/celsius",
    ] {
        report = patch(report, pointer, Value::Null);
    }
    assert_accepted("可选字段全部缺失", report);
}

// ─── 非 JSON 可达的防御分支 ──────────────────────────────────────────────────

/// `validate()` 里的 `is_finite()` 检查**无法从 HTTP 路径触达**——JSON 语法不支持
/// `NaN` / `Infinity` 字面量，serde_json 在解析阶段就会拒绝。
///
/// 但这些检查并非多余：`AgentReport` 是 pub 类型，字段也是 pub，进程内构造出的
/// 报文同样会走 `validate()`。NaN 若混入，会污染 `metric_summary()` 的 `f64::max`
/// 归并并最终写进数据库。这里直接构造结构体来覆盖这条分支。
#[test]
fn rejects_non_finite_values_from_programmatic_construction() {
    assert!(
        serde_json::from_str::<f64>("NaN").is_err(),
        "前提假设已变化：JSON 现在能承载 NaN，需重新评估 is_finite 检查的可达性"
    );

    for (case, bad) in [
        ("NaN", f64::NAN),
        ("+∞", f64::INFINITY),
        ("-∞", f64::NEG_INFINITY),
    ] {
        let mut report = programmatic_report();
        report.system.cpu.usage_percent = bad;
        assert!(
            report.validate().is_err(),
            "[cpu.usage_percent={case}] 非有限值必须被拒绝"
        );

        let mut report = programmatic_report();
        report.interval_seconds = bad;
        assert!(
            report.validate().is_err(),
            "[interval_seconds={case}] 非有限值必须被拒绝"
        );
    }
}

fn programmatic_report() -> AgentReport {
    AgentReport {
        schema_version: 1,
        report_id: Uuid::new_v4().to_string(),
        collected_at: Utc::now(),
        host: HostIdentity {
            id: Uuid::new_v4().to_string(),
            name: "valid-host".into(),
            os: "linux".into(),
            os_version: None,
            kernel_version: None,
            arch: "x86_64".into(),
            agent_version: "0.3.2".into(),
        },
        interval_seconds: 10.0,
        system: SystemSnapshot {
            uptime_seconds: 1,
            cpu: CpuSnapshot {
                usage_percent: 10.0,
                logical_count: 1,
                physical_count: None,
                per_core_percent: vec![10.0],
            },
            memory: MemorySnapshot {
                total_bytes: 100,
                used_bytes: 50,
                available_bytes: 50,
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
