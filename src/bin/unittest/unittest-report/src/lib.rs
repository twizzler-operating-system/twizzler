use std::{fmt::Display, str::FromStr, time::Duration};

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

impl ReportInfo {
    pub fn failed(&self) -> usize {
        self.tests.iter().filter(|t| !t.status.passed()).count()
    }

    pub fn all_passed(&self) -> bool {
        self.failed() == 0
    }
}

/// How a single test binary finished.
///
/// A test only counts as passed if it ran to completion and exited zero; anything else names what
/// went wrong, so a failure can be reported rather than silently counted as a success.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum TestStatus {
    Passed,
    Failed { code: i32 },
    SpawnFailed { err: String },
    Skipped,
}

impl TestStatus {
    pub fn passed(&self) -> bool {
        matches!(self, TestStatus::Passed)
    }
}

impl Display for TestStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TestStatus::Passed => write!(f, "ok"),
            TestStatus::Failed { code } => write!(f, "FAILED (exit {})", code),
            TestStatus::SpawnFailed { err } => write!(f, "FAILED to start ({})", err),
            TestStatus::Skipped => write!(f, "skipped"),
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct TestResult {
    pub name: String,
    pub status: TestStatus,
    #[serde(default)]
    pub duration: Duration,
}

impl FromStr for Report {
    type Err = serde_json::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(s)
    }
}

impl Report {
    /// Parse a report from the start of `s`, ignoring anything following the JSON value.
    ///
    /// The report shares the guest console with the kernel log, and a kernel line can splice
    /// itself in before the report's newline — one stray byte then makes `from_str` reject an
    /// otherwise complete report, and a run that passed every test is recorded as producing none.
    pub fn from_prefix(s: &str) -> Result<Self, serde_json::Error> {
        serde_json::Deserializer::from_str(s)
            .into_iter::<Self>()
            .next()
            .unwrap_or_else(|| serde_json::from_str::<Self>(""))
    }
}
