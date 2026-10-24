use crate::types::{LogFilter, LogLevel, LogSource};
use chrono::{DateTime, Utc};

pub struct LogFilterBuilder {
    filter: LogFilter,
}

impl LogFilterBuilder {
    pub fn new() -> Self {
        Self {
            filter: LogFilter::default(),
        }
    }

    pub fn level(mut self, level: LogLevel) -> Self {
        self.filter.level = Some(level);
        self
    }

    pub fn source(mut self, source: LogSource) -> Self {
        self.filter.source = Some(source);
        self
    }

    pub fn component(mut self, component: &str) -> Self {
        self.filter.component = Some(component.to_string());
        self
    }

    pub fn stage_id(mut self, stage_id: &str) -> Self {
        self.filter.stage_id = Some(stage_id.to_string());
        self
    }

    pub fn flow_id(mut self, flow_id: &str) -> Self {
        self.filter.flow_id = Some(flow_id.to_string());
        self
    }

    pub fn run_id(mut self, run_id: &str) -> Self {
        self.filter.run_id = Some(run_id.to_string());
        self
    }

    pub fn container_id(mut self, container_id: &str) -> Self {
        self.filter.container_id = Some(container_id.to_string());
        self
    }

    pub fn since(mut self, since: DateTime<Utc>) -> Self {
        self.filter.since = Some(since);
        self
    }

    pub fn until(mut self, until: DateTime<Utc>) -> Self {
        self.filter.until = Some(until);
        self
    }

    pub fn limit(mut self, limit: usize) -> Self {
        self.filter.limit = Some(limit);
        self
    }

    pub fn offset(mut self, offset: usize) -> Self {
        self.filter.offset = Some(offset);
        self
    }

    pub fn search_query(mut self, query: &str) -> Self {
        self.filter.search_query = Some(query.to_string());
        self
    }

    pub fn build(self) -> LogFilter {
        self.filter
    }
}

impl Default for LogFilterBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_builder() {
        let filter = LogFilterBuilder::new()
            .level(LogLevel::Error)
            .source(LogSource::Stage)
            .limit(100)
            .build();

        assert_eq!(filter.level, Some(LogLevel::Error));
        assert_eq!(filter.source, Some(LogSource::Stage));
        assert_eq!(filter.limit, Some(100));
    }
}
