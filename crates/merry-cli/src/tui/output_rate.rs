use merry_core::{ModelOutputRate, RuntimeEvent};

/// Presentation of the last measurable runtime-owned throughput sample.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct OutputRate {
    last_measurable_sample: Option<ModelOutputRate>,
}

impl OutputRate {
    pub(crate) fn observe(&mut self, event: &RuntimeEvent) {
        let RuntimeEvent::ModelOutputRateUpdated {
            rate: Some(rate), ..
        } = event
        else {
            return;
        };
        if rate.tokens_per_second().is_some() || self.last_measurable_sample.is_none() {
            self.last_measurable_sample = Some(*rate);
        }
    }

    pub(crate) fn label(&self) -> String {
        match self
            .last_measurable_sample
            .and_then(|sample| sample.tokens_per_second().map(|rate| (sample, rate)))
        {
            Some((sample, rate)) => {
                let prefix = if sample.is_estimated() { "≈" } else { "" };
                format!("{prefix}{rate:.1} tok/s")
            }
            None => "- tok/s".to_owned(),
        }
    }
}
