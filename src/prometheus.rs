use std::collections::BTreeMap;
use std::fmt::Write as _;

#[derive(Clone, Copy)]
#[allow(dead_code)]
pub enum MetricType {
    Gauge,
    Counter,
    Summary,
    Histogram,
    Untyped,
}
impl MetricType {
    fn as_str(self) -> &'static str {
        match self {
            MetricType::Gauge => "gauge",
            MetricType::Counter => "counter",
            MetricType::Summary => "summary",
            MetricType::Histogram => "histogram",
            MetricType::Untyped => "untyped",
        }
    }
}

#[derive(Clone)]
pub struct Sample {
    pub labels: Vec<(String, String)>,
    pub value: f64,
    pub timestamp_ms: Option<i64>,
}

impl Sample {
    pub fn new(value: f64) -> Self {
        Self {
            labels: Vec::new(),
            value,
            timestamp_ms: None,
        }
    }

    #[allow(dead_code)]
    pub fn with_label(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.labels.push((k.into(), v.into()));
        self
    }

    #[allow(dead_code)]
    pub fn with_ts(mut self, ts_ms: i64) -> Self {
        self.timestamp_ms = Some(ts_ms);
        self
    }
}

struct MetricDef {
    help: Option<String>,
    mtype: Option<MetricType>,
    samples: Vec<Sample>,
}
impl MetricDef {
    fn new() -> Self {
        Self {
            help: None,
            mtype: None,
            samples: Vec::new(),
        }
    }
}

pub struct MetricsBuilder {
    metrics: BTreeMap<String, MetricDef>,
}
impl MetricsBuilder {
    pub fn new() -> Self {
        Self {
            metrics: BTreeMap::new(),
        }
    }

    pub fn set_help(&mut self, name: &str, help: impl Into<String>) -> &mut Self {
        self.metrics
            .entry(name.to_string())
            .or_insert_with(MetricDef::new)
            .help = Some(help.into());
        self
    }

    pub fn set_type(&mut self, name: &str, mtype: MetricType) -> &mut Self {
        self.metrics
            .entry(name.to_string())
            .or_insert_with(MetricDef::new)
            .mtype = Some(mtype);
        self
    }

    pub fn add_sample(&mut self, name: &str, sample: Sample) -> &mut Self {
        self.metrics
            .entry(name.to_string())
            .or_insert_with(MetricDef::new)
            .samples
            .push(sample);
        self
    }

    // Convenience helpers
    pub fn add_gauge(
        &mut self,
        name: &str,
        help: &str,
        value: f64,
        labels: &[(&str, &str)],
        ts_ms: Option<i64>,
    ) -> &mut Self {
        self.set_help(name, help).set_type(name, MetricType::Gauge);
        let mut s = Sample::new(value);
        s.labels = labels
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        s.timestamp_ms = ts_ms;
        self.add_sample(name, s)
    }

    #[allow(dead_code)]
    pub fn add_counter(
        &mut self,
        name: &str,
        help: &str,
        value: f64,
        labels: &[(&str, &str)],
        ts_ms: Option<i64>,
    ) -> &mut Self {
        self.set_help(name, help)
            .set_type(name, MetricType::Counter);
        let mut s = Sample::new(value);
        s.labels = labels
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        s.timestamp_ms = ts_ms;
        self.add_sample(name, s)
    }

    #[allow(dead_code)]
    pub fn add_untyped(
        &mut self,
        name: &str,
        help: &str,
        value: f64,
        labels: &[(&str, &str)],
        ts_ms: Option<i64>,
    ) -> &mut Self {
        self.set_help(name, help)
            .set_type(name, MetricType::Untyped);
        let mut s = Sample::new(value);
        s.labels = labels
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        s.timestamp_ms = ts_ms;
        self.add_sample(name, s)
    }

    /// Render to Prometheus text exposition format.
    pub fn build(self) -> String {
        let mut out = String::with_capacity(1024);
        for (name, def) in self.metrics {
            if let Some(help) = &def.help {
                let _ = writeln!(&mut out, "# HELP {} {}", name, escape_help(help));
            }
            if let Some(mt) = def.mtype {
                let _ = writeln!(&mut out, "# TYPE {} {}", name, mt.as_str());
            }
            for s in def.samples {
                let _ = write!(&mut out, "{}", name);
                if !s.labels.is_empty() {
                    let _ = write!(&mut out, "{{");
                    for (i, (k, v)) in s.labels.iter().enumerate() {
                        if i > 0 {
                            let _ = write!(&mut out, ",");
                        }
                        let _ = write!(&mut out, "{}=\"{}\"", k, escape_label(v));
                    }
                    let _ = write!(&mut out, "}}");
                }
                let _ = write!(&mut out, " {}", format_value(s.value));
                if let Some(ts) = s.timestamp_ms {
                    let _ = write!(&mut out, " {}", ts);
                }
                let _ = writeln!(&mut out);
            }
        }
        out
    }
}

fn escape_label(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str(r#"\\"#),
            '"' => out.push_str(r#"\""#),
            '\n' => out.push_str(r#"\n"#),
            _ => out.push(ch),
        }
    }
    out
}
fn escape_help(s: &str) -> String {
    s.replace('\n', r"\n")
}
fn format_value(v: f64) -> String {
    if v.is_nan() {
        "NaN".to_string()
    } else if v.is_infinite() {
        if v.is_sign_positive() {
            "+Inf".into()
        } else {
            "-Inf".into()
        }
    } else {
        format!("{}", v)
    }
}
