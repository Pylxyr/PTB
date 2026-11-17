//! Parsers for the handful of Linux `/proc` and `/etc` files the probe reads. They take the file's text, so
//! they are tested with canned samples on any machine, and the caller decides what a missing file means.

/// `MemTotal` and `MemAvailable` from `/proc/meminfo`, in KiB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemInfo {
    pub total_kib: u64,
    pub available_kib: Option<u64>,
}

/// Cumulative CPU time counters (clock ticks) from the aggregate `cpu` line of `/proc/stat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuTimes {
    pub user: u64,
    pub nice: u64,
    pub system: u64,
    pub idle: u64,
    pub iowait: u64,
    pub irq: u64,
    pub softirq: u64,
    pub steal: u64,
}

impl CpuTimes {
    /// Every counter that is time spent, idle and stolen time included.
    pub fn total(&self) -> u64 {
        self.user
            + self.nice
            + self.system
            + self.idle
            + self.iowait
            + self.irq
            + self.softirq
            + self.steal
    }
}

fn kib_value(line: &str, key: &str) -> Option<u64> {
    let rest = line.strip_prefix(key)?.trim_start_matches(':').trim();
    rest.split_whitespace().next()?.parse().ok()
}

pub fn parse_meminfo(text: &str) -> Option<MemInfo> {
    let mut total = None;
    let mut available = None;
    for line in text.lines() {
        if let Some(v) = kib_value(line, "MemTotal") {
            total = Some(v);
        } else if let Some(v) = kib_value(line, "MemAvailable") {
            available = Some(v);
        }
    }
    Some(MemInfo {
        total_kib: total?,
        available_kib: available,
    })
}

/// The first `model name` of `/proc/cpuinfo`, whitespace collapsed.
pub fn parse_cpu_model(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim() == "model name").then(|| value.split_whitespace().collect::<Vec<_>>().join(" "))
    })
}

/// The aggregate `cpu ` line of `/proc/stat`. Older kernels list fewer counters; missing ones count as 0.
pub fn parse_cpu_times(text: &str) -> Option<CpuTimes> {
    let line = text.lines().find(|l| l.starts_with("cpu "))?;
    let mut n = line
        .split_whitespace()
        .skip(1)
        .map(|v| v.parse::<u64>().ok());
    let mut next = || n.next().flatten().unwrap_or(0);
    let times = CpuTimes {
        user: next(),
        nice: next(),
        system: next(),
        idle: next(),
        iowait: next(),
        irq: next(),
        softirq: next(),
        steal: next(),
    };
    (times.total() > 0).then_some(times)
}

/// Share of CPU time the hypervisor took away between two readings, in percent. `None` if no time passed.
pub fn steal_percent(before: &CpuTimes, after: &CpuTimes) -> Option<f64> {
    let total = after.total().checked_sub(before.total())?;
    if total == 0 {
        return None;
    }
    let steal = after.steal.saturating_sub(before.steal);
    Some(steal as f64 * 100.0 / total as f64)
}

/// Peak resident memory (`VmHWM`) and current resident memory (`VmRSS`) of this process from
/// `/proc/self/status`, in KiB: `(peak, current)`.
pub fn parse_self_memory(text: &str) -> (Option<u64>, Option<u64>) {
    let mut peak = None;
    let mut current = None;
    for line in text.lines() {
        if let Some(v) = kib_value(line, "VmHWM") {
            peak = Some(v);
        } else if let Some(v) = kib_value(line, "VmRSS") {
            current = Some(v);
        }
    }
    (peak, current)
}

/// `PRETTY_NAME` of `/etc/os-release`, quotes removed.
pub fn parse_os_pretty_name(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let value = line.strip_prefix("PRETTY_NAME=")?;
        Some(value.trim().trim_matches('"').to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEMINFO: &str = "MemTotal:         958292 kB\nMemFree:          101312 kB\nMemAvailable:     640120 kB\nBuffers: 1 kB\n";
    const STAT: &str =
        "cpu  1000 5 400 8000 20 0 10 65 0 0\ncpu0 500 2 200 4000 10 0 5 30 0 0\nintr 1\n";

    #[test]
    fn meminfo_reads_total_and_available() {
        let m = parse_meminfo(MEMINFO).unwrap();
        assert_eq!(
            m,
            MemInfo {
                total_kib: 958_292,
                available_kib: Some(640_120)
            }
        );
        assert_eq!(
            parse_meminfo("MemTotal: 5 kB\n").unwrap().available_kib,
            None
        );
        assert_eq!(parse_meminfo("nothing useful"), None);
    }

    #[test]
    fn cpu_model_collapses_whitespace() {
        let text = "processor\t: 0\nmodel name\t: AMD EPYC   7551 32-Core Processor\ncpu MHz : 1\n";
        assert_eq!(
            parse_cpu_model(text).as_deref(),
            Some("AMD EPYC 7551 32-Core Processor")
        );
        assert_eq!(parse_cpu_model("flags : a b"), None);
    }

    #[test]
    fn cpu_times_use_the_aggregate_line_only() {
        let t = parse_cpu_times(STAT).unwrap();
        assert_eq!((t.user, t.idle, t.steal), (1000, 8000, 65));
        assert_eq!(t.total(), 9500);
        assert_eq!(parse_cpu_times("cpu0 1 2 3 4\n"), None);
    }

    #[test]
    fn short_cpu_lines_default_missing_counters_to_zero() {
        let t = parse_cpu_times("cpu  10 0 5 85\n").unwrap();
        assert_eq!((t.iowait, t.steal), (0, 0));
        assert_eq!(parse_cpu_times("cpu  0 0 0 0\n"), None);
    }

    #[test]
    fn steal_percent_is_the_stolen_share_of_elapsed_time() {
        let a = parse_cpu_times("cpu  100 0 100 800 0 0 0 0\n").unwrap();
        let b = parse_cpu_times("cpu  150 0 150 900 0 0 0 100\n").unwrap();
        // 300 ticks elapsed, 100 of them stolen.
        let pct = steal_percent(&a, &b).unwrap();
        assert!((pct - 33.333).abs() < 0.01, "{pct}");
        assert_eq!(steal_percent(&a, &a), None);
        assert_eq!(
            steal_percent(&b, &a),
            None,
            "counters that went backwards are not a reading"
        );
    }

    #[test]
    fn self_memory_reads_peak_and_current() {
        let text = "Name:\tx\nVmHWM:\t   12345 kB\nVmRSS:\t    9000 kB\n";
        assert_eq!(parse_self_memory(text), (Some(12_345), Some(9_000)));
        assert_eq!(parse_self_memory("Name: x"), (None, None));
    }

    #[test]
    fn os_name_is_unquoted() {
        let text = "NAME=\"Ubuntu\"\nPRETTY_NAME=\"Ubuntu 24.04.5 LTS\"\n";
        assert_eq!(
            parse_os_pretty_name(text).as_deref(),
            Some("Ubuntu 24.04.5 LTS")
        );
        assert_eq!(parse_os_pretty_name("ID=x"), None);
    }
}
