//! The phone's SoC, and which compiled model and QNN generation go with it.
pub struct Soc {
    pub model: String,
    pub vendor: String,
    pub htp: Option<u32>,
}

pub fn this_phone() -> Soc {
    let prop = |name: &str| {
        std::process::Command::new("getprop")
            .arg(name)
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let model = std::env::var("NPU_BENCH_SOC").unwrap_or_else(|_| prop("ro.soc.model"));
    let htp = match model.as_str() {
        "SM8450" | "SM8475" => Some(69),
        "SM8550" => Some(73),
        "SM8650" => Some(75),
        "SM8750" => Some(79),
        "SM8845" | "SM8850" => Some(81),
        _ => None,
    };
    Soc { model, vendor: prop("ro.soc.manufacturer"), htp }
}
