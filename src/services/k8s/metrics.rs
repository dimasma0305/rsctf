/// Parse a Kubernetes CPU quantity into cores.
pub(super) fn parse_cpu_cores(value: &str) -> Option<f64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Some(number) = value.strip_suffix('n') {
        number.trim().parse::<f64>().ok().map(|n| n / 1e9)
    } else if let Some(number) = value.strip_suffix('u') {
        number.trim().parse::<f64>().ok().map(|n| n / 1e6)
    } else if let Some(number) = value.strip_suffix('m') {
        number.trim().parse::<f64>().ok().map(|n| n / 1e3)
    } else {
        value.parse::<f64>().ok()
    }
}

/// Parse a Kubernetes memory quantity into bytes.
pub(super) fn parse_memory_bytes(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    const SUFFIXES: &[(&str, f64)] = &[
        ("Ki", 1024.0),
        ("Mi", 1_048_576.0),
        ("Gi", 1_073_741_824.0),
        ("Ti", 1_099_511_627_776.0),
        ("Pi", 1_125_899_906_842_624.0),
        ("Ei", 1_152_921_504_606_846_976.0),
        ("k", 1e3),
        ("M", 1e6),
        ("G", 1e9),
        ("T", 1e12),
        ("P", 1e15),
        ("E", 1e18),
    ];
    for (suffix, multiplier) in SUFFIXES {
        if let Some(number) = value.strip_suffix(suffix) {
            return number
                .trim()
                .parse::<f64>()
                .ok()
                .filter(|number| number.is_finite() && *number >= 0.0)
                .map(|number| (number * multiplier) as u64);
        }
    }
    value
        .parse::<f64>()
        .ok()
        .filter(|number| number.is_finite() && *number >= 0.0)
        .map(|number| number as u64)
}
