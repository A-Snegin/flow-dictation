//! Exact percentiles over small sample sets. Averages are never reported.

#[derive(Default, Clone)]
pub struct Samples {
    values_ms: Vec<f64>,
}

impl Samples {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, d: std::time::Duration) {
        self.values_ms.push(d.as_secs_f64() * 1000.0);
    }

    pub fn push_ms(&mut self, ms: f64) {
        self.values_ms.push(ms);
    }

    pub fn len(&self) -> usize {
        self.values_ms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values_ms.is_empty()
    }

    /// Nearest-rank percentile, p in 0.0..=1.0.
    pub fn p(&self, p: f64) -> f64 {
        if self.values_ms.is_empty() {
            return f64::NAN;
        }
        let mut v = self.values_ms.clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let rank = (p * v.len() as f64).ceil().max(1.0) as usize;
        v[rank.min(v.len()) - 1]
    }

    pub fn max(&self) -> f64 {
        self.p(1.0)
    }

    pub fn report(&self, label: &str) -> String {
        if self.is_empty() {
            return format!("{label:<34} no samples");
        }
        format!(
            "{label:<34} n={:<5} p50={:>7.1} p90={:>7.1} p95={:>7.1} p99={:>7.1} max={:>7.1}  (ms)",
            self.len(),
            self.p(0.50),
            self.p(0.90),
            self.p(0.95),
            self.p(0.99),
            self.max()
        )
    }
}
