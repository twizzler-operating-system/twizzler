use std::{str::FromStr, time::Duration};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct Report {
    pub status: ReportStatus,
}

impl Report {
    pub fn pending() -> Self {
        Self {
            status: ReportStatus::Pending,
        }
    }

    pub fn ready(info: ReportInfo) -> Self {
        Self {
            status: ReportStatus::Ready(info),
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub enum ReportStatus {
    Pending,
    Ready(ReportInfo),
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ReportInfo {
    pub time: Duration,
    pub tests: Vec<TestResult>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct TestResult {
    pub name: String,
    pub passed: bool,
}

impl FromStr for Report {
    type Err = serde_json::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(s)
    }
}
