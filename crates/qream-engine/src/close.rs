use crate::{Error, Result, storage};
use chrono::{DateTime, Duration, FixedOffset};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

/// The registration gate shared by every front. A draft's null close is not a declaration.
pub fn require_declared(root: &Path) -> Result<()> {
    let path = root.join("CLOSE.json");
    let value: serde_json::Value = storage::read(&path).map_err(|error| match error {
        Error::Io { ref source, .. } if source.kind() == std::io::ErrorKind::NotFound =>
            Error::Denied("Declare the last wave and expected close in CLOSE.json before registering the campaign".into()),
        Error::Json(_) => Error::Denied("The declared close must be valid JSON with a last wave and expected close".into()),
        other => other,
    })?;
    let last_wave = value["last_wave"]
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .unwrap_or(0);
    validate_declaration(last_wave, value["expected_close"].as_str().unwrap_or(""))
}

pub fn validate_declaration(last_wave: u32, expected_close: &str) -> Result<()> {
    if last_wave == 0 || when::parse(expected_close).is_none() {
        return Err(Error::Denied("Declare a positive last wave and an expected close time with its UTC offset before registering the campaign".into()));
    }
    Ok(())
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Close {
    pub campaign: String,
    pub last_wave: u32,
    #[serde(deserialize_with = "when::exact")]
    pub declared_at: DateTime<FixedOffset>,
    #[serde(deserialize_with = "when::exact")]
    pub expected_close: DateTime<FixedOffset>,
    pub by: String,
    pub rule: String,
    #[serde(
        default,
        deserialize_with = "when::optional",
        skip_serializing_if = "Option::is_none"
    )]
    pub actual_close: Option<DateTime<FixedOffset>>,
    #[serde(
        default,
        deserialize_with = "when::optional",
        skip_serializing_if = "Option::is_none"
    )]
    pub previous_estimate: Option<DateTime<FixedOffset>>,
}
pub struct Pace {
    pub now: DateTime<FixedOffset>,
    pub accepted_per_hour: f64,
    pub remaining_tasks: u32,
    pub waves_remaining: u32,
    pub build_test_seconds_per_wave: f64,
}
impl Close {
    pub fn read(root: &Path) -> Result<Self> {
        let close: Self = storage::read(&root.join("CLOSE.json"))?;
        close.allow_wave(close.last_wave)?;
        Ok(close)
    }
    pub fn write(&self, root: &Path) -> Result<()> {
        self.allow_wave(self.last_wave)?;
        storage::write(&root.join("CLOSE.json"), self)
    }
    pub fn allow_wave(&self, wave: u32) -> Result<()> {
        if self.last_wave == 0 || wave == 0 || wave > self.last_wave {
            return Err(Error::Denied(format!(
                "Cannot add wave {wave}: this campaign closes after wave {}",
                self.last_wave
            )));
        }
        Ok(())
    }
    pub fn expected_close(&mut self, pace: Pace) -> Result<DateTime<FixedOffset>> {
        if !pace.accepted_per_hour.is_finite()
            || pace.accepted_per_hour < 0.0
            || !pace.build_test_seconds_per_wave.is_finite()
            || pace.build_test_seconds_per_wave < 0.0
        {
            return Err(Error::Denied(
                "pace must contain finite nonnegative measurements".into(),
            ));
        }
        if pace.accepted_per_hour == 0.0 && pace.remaining_tasks > 0 {
            return Ok(self.expected_close);
        }
        let task_seconds = if pace.remaining_tasks == 0 {
            0.0
        } else {
            f64::from(pace.remaining_tasks) * 3600.0 / pace.accepted_per_hour
        };
        let seconds = (task_seconds
            + f64::from(pace.waves_remaining) * pace.build_test_seconds_per_wave)
            .ceil();
        if seconds > i64::MAX as f64 {
            return Err(Error::Denied(
                "pace exceeds the supported time range".into(),
            ));
        }
        let next = pace
            .now
            .checked_add_signed(
                Duration::try_seconds(seconds as i64)
                    .ok_or_else(|| Error::Denied("pace exceeds the supported duration".into()))?,
            )
            .ok_or_else(|| Error::Denied("close exceeds the supported time range".into()))?;
        if next != self.expected_close {
            self.previous_estimate = Some(self.expected_close);
            self.expected_close = next;
        }
        Ok(next)
    }
}

/// The close's moving estimate, kept beside the declaration in `state/close-estimate.json`;
/// CLOSE.json keeps what was declared at launch.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Estimate {
    pub expected: Option<DateTime<FixedOffset>>,
    pub previous: Option<DateTime<FixedOffset>>,
}
/// A move smaller than this keeps the shown estimate and its previous value as they are.
pub const MOVE_SECONDS: i64 = 300;
impl Estimate {
    pub fn path(root: &Path) -> std::path::PathBuf {
        root.join("state/close-estimate.json")
    }
    pub fn read(root: &Path) -> Result<Self> {
        storage::read_json(&Self::path(root))
    }
    pub fn write(&self, root: &Path) -> Result<()> {
        storage::write_json(&Self::path(root), self)
    }
    /// Takes a new estimate; a move of more than five minutes keeps the old one as previous.
    pub fn advance(&mut self, next: DateTime<FixedOffset>) -> bool {
        match self.expected {
            Some(current) if (next - current).num_seconds().abs() <= MOVE_SECONDS => false,
            Some(current) => {
                self.previous = Some(current);
                self.expected = Some(next);
                true
            }
            None => {
                self.expected = Some(next);
                true
            }
        }
    }
}

/// One task as the pace sees it, from the manifest, the control ledger and its seal.
#[derive(Clone, Debug)]
pub struct PaceTask {
    pub wave: u32,
    pub kind: String,
    pub started: Option<DateTime<FixedOffset>>,
    pub sealed: Option<DateTime<FixedOffset>>,
    pub accepted: Option<DateTime<FixedOffset>>,
    pub cancelled: bool,
}

fn median(mut values: Vec<i64>) -> Option<i64> {
    values.sort_unstable();
    let n = values.len();
    match n {
        0 => None,
        _ if n % 2 == 1 => Some(values[n / 2]),
        _ => Some((values[n / 2 - 1] + values[n / 2]) / 2),
    }
}

/// The expected close from the campaign's own pace: each wave left takes its longest
/// remaining task, judged by the median accepted-task time of that task's kind (all kinds
/// when the kind has none yet), plus the median review gap from seal to acceptance.
/// None until some task has been accepted, or when nothing is left.
pub fn pace_close(tasks: &[PaceTask], now: DateTime<FixedOffset>) -> Option<DateTime<FixedOffset>> {
    let done: Vec<&PaceTask> = tasks.iter().filter(|t| t.accepted.is_some()).collect();
    let work = |t: &&PaceTask| -> Option<i64> {
        let end = t.sealed.or(t.accepted)?;
        Some((end - t.started?).num_seconds().max(0))
    };
    let all_work = median(done.iter().filter_map(work).collect())?;
    let review = median(
        done.iter()
            .filter_map(|t| Some((t.accepted? - t.sealed?).num_seconds().max(0)))
            .collect(),
    )
    .unwrap_or(0);
    let mut waves: BTreeMap<u32, i64> = BTreeMap::new();
    for t in tasks
        .iter()
        .filter(|t| t.accepted.is_none() && !t.cancelled)
    {
        let remaining = if t.sealed.is_some() {
            0
        } else {
            let typical = median(
                done.iter()
                    .filter(|d| d.kind == t.kind)
                    .filter_map(work)
                    .collect(),
            )
            .unwrap_or(all_work);
            let spent = t.started.map_or(0, |s| (now - s).num_seconds().max(0));
            (typical - spent).max(0)
        };
        let wave = waves.entry(t.wave).or_insert(0);
        *wave = (*wave).max(remaining);
    }
    if waves.is_empty() {
        return None;
    }
    let seconds: i64 = waves.values().map(|w| w + review).sum();
    now.checked_add_signed(Duration::try_seconds(seconds)?)
}

/// CLOSE.json times as the skill writes them: RFC 3339, with or without seconds.
pub mod when {
    use chrono::{DateTime, FixedOffset};
    use serde::{Deserialize, Deserializer, de::Error};
    pub fn parse(text: &str) -> Option<DateTime<FixedOffset>> {
        DateTime::parse_from_rfc3339(text)
            .ok()
            .or_else(|| DateTime::parse_from_str(text, "%Y-%m-%dT%H:%M%:z").ok())
    }
    pub fn exact<'d, D: Deserializer<'d>>(d: D) -> Result<DateTime<FixedOffset>, D::Error> {
        let text = String::deserialize(d)?;
        parse(&text).ok_or_else(|| D::Error::custom(format!("not a time: {text}")))
    }
    pub fn optional<'d, D: Deserializer<'d>>(
        d: D,
    ) -> Result<Option<DateTime<FixedOffset>>, D::Error> {
        match Option::<String>::deserialize(d)? {
            None => Ok(None),
            Some(text) => parse(&text)
                .map(Some)
                .ok_or_else(|| D::Error::custom(format!("not a time: {text}"))),
        }
    }
    #[cfg(test)]
    mod tests {
        #[test]
        fn minutes_without_seconds_parse() {
            let at = super::parse("2026-10-10T12:30-04:00").expect("skill time");
            assert_eq!(at.to_rfc3339(), "2026-10-10T12:30:00-04:00");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(minute: i64) -> DateTime<FixedOffset> {
        when::parse("2026-10-10T12:00:00-04:00").expect("fixed time") + Duration::minutes(minute)
    }
    fn task(
        wave: u32,
        kind: &str,
        started: i64,
        sealed: Option<i64>,
        accepted: Option<i64>,
    ) -> PaceTask {
        PaceTask {
            wave,
            kind: kind.into(),
            started: Some(at(started)),
            sealed: sealed.map(at),
            accepted: accepted.map(at),
            cancelled: false,
        }
    }

    #[test]
    fn the_close_moves_with_pace_and_keeps_the_previous_estimate() {
        // Two accepted tasks: 30 and 40 minutes of work (median 35), review gaps 4 and 6 (median 5).
        let mut tasks = vec![
            task(1, "claude", 0, Some(30), Some(34)),
            task(1, "claude", 0, Some(40), Some(46)),
            task(2, "claude", 46, None, None),
            task(3, "claude", 46, None, None),
        ];
        let now = at(50);
        // Wave 2: 35 - 4 = 31 left + 5 review; wave 3: 31 + 5. 72 minutes from 12:50.
        let first = pace_close(&tasks, now).expect("an estimate");
        assert_eq!(first, at(50 + 72));
        let mut estimate = Estimate {
            expected: Some(at(180)),
            previous: None,
        };
        assert!(estimate.advance(first), "a 58-minute move is taken");
        assert_eq!(estimate.previous, Some(at(180)));
        assert_eq!(estimate.expected, Some(at(122)));
        // Two minutes later at the same pace the estimate stays: no churn under five minutes.
        assert!(
            !estimate.advance(pace_close(&tasks, at(52)).expect("estimate") + Duration::minutes(2))
        );
        assert_eq!(estimate.previous, Some(at(180)));
        // A slow task: wave 2 seals late, so the pace (median work) grows and the close moves out.
        tasks[2].sealed = Some(at(120));
        tasks[2].accepted = Some(at(130));
        let later = pace_close(&tasks, at(130)).expect("estimate");
        // Work 30, 40, 74 → median 40; reviews 4, 6, 10 → median 6; wave 3 has 40 - 84 < 0 → 0 + 6.
        assert_eq!(later, at(136));
        assert!(estimate.advance(later));
        assert_eq!(estimate.previous, Some(at(122)));
        assert_eq!(estimate.expected, Some(at(136)));
        println!(
            "CLOSE expected {} (was {})",
            estimate
                .expected
                .map(|e| e.format("%H:%M").to_string())
                .unwrap_or_default(),
            estimate
                .previous
                .map(|e| e.format("%H:%M").to_string())
                .unwrap_or_default()
        );
    }

    #[test]
    fn no_pace_without_an_accepted_task_and_none_when_nothing_is_left() {
        assert!(pace_close(&[task(1, "codex", 0, None, None)], at(5)).is_none());
        assert!(pace_close(&[task(1, "codex", 0, Some(9), Some(10))], at(11)).is_none());
        let mut cancelled = task(2, "codex", 0, None, None);
        cancelled.cancelled = true;
        assert!(pace_close(&[task(1, "codex", 0, Some(9), Some(10)), cancelled], at(11)).is_none());
    }
}
