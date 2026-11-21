//! This tool's own cloud-recognition cost, as a user-visible summary.
//!
//! The durable trial ledger in [`crate::cloud`] already records every paid request
//! and its reservation. This module only turns that record into facts a user can
//! read, and it deliberately separates three things that must never be collapsed:
//!
//! - **confirmed** usage: the provider reported final cumulative usage for the
//!   request and the verified price was applied;
//! - **conservative** estimate: the request has no final usage yet, so the whole
//!   reservation stays committed (the upper bound, not a guess at the real cost);
//! - **unknown**: the actual usage or the actual billed charge cannot be proven,
//!   so the request is counted as unknown instead of being displayed as zero.
//!
//! # What the numbers do and do not cover
//!
//! They cover recognition requests **this tool** sent with the configured
//! provider, model and region. They do not cover anything else billed to the same
//! cloud account, and they are estimates, not a bill.
//!
//! # Two independent limits
//!
//! [`UsageSummary::trial_cap_nanoyuan`] is the trial's hard spending cap (CNY 50);
//! it is enforced by the ledger before a request is admitted.
//! [`UsageSummary::MONTH_REMINDER_NANOYUAN`] (CNY 100) is a *reminder line only*:
//! nothing here can grant, extend or raise budget, and approaching or reaching it
//! never stops a capture. The reminder fires at
//! [`UsageSummary::REMINDER_THRESHOLD_NANOYUAN`] (90% of the line, CNY 90) so the
//! user is warned while approaching it, not only after crossing it. The two limits
//! must never be confused.
//!
//! # Months
//!
//! Months are UTC calendar months, because the ledger stores a Unix timestamp and
//! this crate has no time-zone database. The panel says "UTC" so the bucket is not
//! mistaken for a bill period. A reminder line tolerates that boundary shift; a
//! bill would not, which is why this is never presented as one.

use std::time::{SystemTime, UNIX_EPOCH};

/// Cumulative usage and cost of this tool's own cloud recognition requests.
///
/// Nanoyuan keeps every amount exact and integer (1 CNY = 1_000_000_000), matching
/// the ledger. All fields are additive: a consumer that does not understand a new
/// figure still shows the older ones.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UsageSummary {
    /// Every reserved request, including retries of the same recording.
    pub requests: u64,
    /// Requests for which the provider reported complete final usage.
    pub settled_requests: u64,
    /// Confirmed cost: final provider usage priced with the verified price source.
    /// A final free-only request contributes 0 here, but is still counted as
    /// unknown below because its actual billed charge was never verified.
    pub confirmed_nanoyuan: u64,
    /// Conservative cost: still-reserved amounts for requests without final usage.
    pub estimated_nanoyuan: u64,
    /// Requests whose actual usage or actual billed charge is not verified. A
    /// request can be both conservatively estimated and unknown; it is never
    /// counted as zero cost.
    pub unknown_requests: u64,
    /// Recordings with more than one request (automatic rebuilds and explicit
    /// retries). Their extra requests are part of `requests`.
    pub retried_segments: u64,
    /// Requests recorded before this summary could date them. They are included in
    /// the lifetime totals but cannot be assigned to a month.
    pub undated_requests: u64,
    /// Confirmed cost of requests recorded in the current UTC month.
    pub month_confirmed_nanoyuan: u64,
    /// Conservative cost of requests recorded in the current UTC month.
    pub month_estimated_nanoyuan: u64,
    /// Unknown requests recorded in the current UTC month.
    pub month_unknown_requests: u64,
    /// The trial's hard cap, copied from the ledger this summary was read from.
    pub trial_cap_nanoyuan: u64,
    /// Confirmed input tokens of settled requests, when the provider reported them.
    pub confirmed_input_tokens: u64,
    /// Confirmed output tokens of settled requests, when the provider reported them.
    pub confirmed_output_tokens: u64,
    /// The ledger could not be read, so every figure above is unknown rather than
    /// zero. Callers must not present this as "no usage".
    pub unreadable: bool,
}

impl UsageSummary {
    /// The CNY 100 monthly reminder line. It is a reminder, never a budget.
    pub const MONTH_REMINDER_NANOYUAN: u64 = 100 * 1_000_000_000;

    /// The point at which the monthly reminder fires: 90% of the line (CNY 90).
    /// Warning while approaching the line is the point; the line itself changes
    /// nothing.
    pub const REMINDER_THRESHOLD_NANOYUAN: u64 = Self::MONTH_REMINDER_NANOYUAN * 9 / 10;

    /// The summary for a ledger that could not be read. The trial cap is still
    /// known from the code, but no spend can be claimed.
    pub fn unreadable(trial_cap_nanoyuan: u64) -> Self {
        Self {
            trial_cap_nanoyuan,
            unreadable: true,
            ..Self::default()
        }
    }

    /// Confirmed plus conservatively held cost over the ledger's whole life.
    pub fn lifetime_nanoyuan(&self) -> u64 {
        self.confirmed_nanoyuan
            .saturating_add(self.estimated_nanoyuan)
    }

    /// Confirmed plus conservatively held cost recorded this UTC month.
    pub fn month_nanoyuan(&self) -> u64 {
        self.month_confirmed_nanoyuan
            .saturating_add(self.month_estimated_nanoyuan)
    }

    /// Whether the current month is approaching the CNY 100 reminder line. The
    /// threshold is 90% of the line; this is a statement about recorded estimates,
    /// never a budget decision.
    pub fn reminder_reached(&self) -> bool {
        !self.unreadable && self.month_nanoyuan() >= Self::REMINDER_THRESHOLD_NANOYUAN
    }
}

/// The current UTC calendar month as `(year, month)`.
pub(crate) fn current_utc_month() -> (i64, u32) {
    utc_month(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs())
            .unwrap_or(0),
    )
}

/// The UTC calendar month of a Unix epoch second, as `(year, month)`.
///
/// Only whole days matter here, so the civil-date conversion is the standard
/// proleptic-Gregorian days-to-date algorithm; no time-zone data is involved.
pub(crate) fn utc_month(seconds: u64) -> (i64, u32) {
    let (year, month, _day) = civil_from_days((seconds / 86_400) as i64);
    (year, month)
}

/// Days since 1970-01-01 to `(year, month, day)` in the proleptic Gregorian
/// calendar (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    (
        if month <= 2 { year + 1 } else { year },
        month as u32,
        day as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_monthly_reminder_fires_while_approaching_the_line() {
        let mut usage = UsageSummary::default();
        usage.month_confirmed_nanoyuan = UsageSummary::REMINDER_THRESHOLD_NANOYUAN - 1;
        assert!(
            !usage.reminder_reached(),
            "just below 90% of the line must not remind yet"
        );
        // 90% of CNY 100 is CNY 90: warned before the line, not only at it.
        assert_eq!(UsageSummary::REMINDER_THRESHOLD_NANOYUAN, 90_000_000_000);
        usage.month_confirmed_nanoyuan = UsageSummary::REMINDER_THRESHOLD_NANOYUAN;
        assert!(usage.reminder_reached());
        usage.month_confirmed_nanoyuan = UsageSummary::MONTH_REMINDER_NANOYUAN;
        assert!(usage.reminder_reached());
        usage.month_estimated_nanoyuan = 1;
        usage.month_confirmed_nanoyuan = UsageSummary::REMINDER_THRESHOLD_NANOYUAN - 1;
        assert!(
            usage.reminder_reached(),
            "confirmed plus conservative estimate is the monthly figure"
        );
        usage.unreadable = true;
        assert!(
            !usage.reminder_reached(),
            "an unreadable ledger can never claim a reached reminder"
        );
    }
}
