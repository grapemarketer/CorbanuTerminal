//! The `/cost` range view: which requested buckets are in progress or
//! recorded through the ledger checkpoint, and the pages that show a range.

use std::sync::Arc;

use codex_state::accounting::EmptyDay;
use codex_state::accounting::InspectionDay;
use codex_state::accounting::InspectionGrouping;
use codex_state::accounting::InspectionRange;

use super::DAY_COVERED;
use super::EstimateGaps;
use super::InspectorPage;
use super::billed_line;
use super::estimate;
use super::inspection_pages;
use super::inspection_pages_for;
use super::interval;
use super::names_accounts;
use super::recording_gap_line;
use super::scope;
use super::unpriced_rows;
use super::utc_date;
use super::utc_instant;

/// Calendar progress and complete accounting through the ledger checkpoint
/// are separate. Request/retention clipping and unverified detail stay partial.
#[derive(Clone, Copy)]
enum BucketProgress {
    /// Still running: everything recorded up to this coverage end.
    InProgress(i64),
    /// A whole requested bucket has all its recorded detail through this
    /// inclusive checkpoint as of this read, even if the calendar interval
    /// has already ended or its first request has not been recorded yet. A
    /// request dispatched at or before the checkpoint can still be written
    /// later (#1009), so this holds only as of the read.
    RecordedThrough(i64),
    /// Starts after the read time.
    NotStarted,
}

fn progress(
    requested: InspectionRange,
    read_at: i64,
    completed_as_of_ms: Option<i64>,
    bucket: &codex_state::accounting::InspectionBucket,
) -> Option<BucketProgress> {
    if bucket.start_ms > read_at {
        return Some(BucketProgress::NotStarted);
    }
    if let Some((start, end)) = bucket.effective
        && bucket.partial
        && bucket.end_ms > read_at
        && requested.end_ms > read_at
        && start == bucket.start_ms
        && bucket
            .days
            .iter()
            .zip(start / 86_400_000..)
            .all(|(state, day)| match state {
                InspectionDay::Ready(_) => true,
                InspectionDay::CheckpointLag => day * 86_400_000 >= end,
                InspectionDay::Range { .. }
                | InspectionDay::Absent
                | InspectionDay::MissingThread
                | InspectionDay::NeedsRefresh
                | InspectionDay::TooLarge
                | InspectionDay::DetailUnavailable { .. } => false,
            })
    {
        return Some(BucketProgress::InProgress(end));
    }
    let checkpoint = completed_as_of_ms?;
    let end = bucket.end_ms.min(checkpoint.saturating_add(1));
    (bucket.partial
        && !bucket.days.is_empty()
        && checkpoint <= read_at
        && requested.start_ms <= bucket.start_ms
        && requested.end_ms >= bucket.end_ms
        && bucket.effective == (bucket.start_ms < end).then_some((bucket.start_ms, end))
        && bucket
            .days
            .iter()
            .zip(bucket.start_ms / 86_400_000..)
            .all(|(state, day)| match state {
                InspectionDay::Ready(view) => {
                    view.coverage.completed_as_of_ms == checkpoint
                        && view.read_at_ms == read_at
                        && view.coverage.aggregate_day_floor * 86_400_000 <= bucket.start_ms
                }
                // Only days strictly after the authoritative checkpoint can
                // lack detail without hiding a genuine inspection shortfall.
                InspectionDay::CheckpointLag => day * 86_400_000 > checkpoint,
                InspectionDay::Range { .. }
                | InspectionDay::Absent
                | InspectionDay::MissingThread
                | InspectionDay::NeedsRefresh
                | InspectionDay::TooLarge
                | InspectionDay::DetailUnavailable { .. } => false,
            }))
    .then_some(BucketProgress::RecordedThrough(checkpoint))
}

pub(super) const NOT_STARTED: &str = "Not started yet — it starts after this view was read.";
pub(super) const NOT_CURRENT: &str = "Snapshot is not current; newer activity is unverified";

fn in_progress_line(end: i64) -> String {
    format!(
        "In progress — totals so far, recorded through {}",
        utc_instant(end - 1)
    )
}

fn recorded_through_line(checkpoint: i64) -> String {
    format!(
        "Recorded through ledger checkpoint: {} (as of this read)",
        utc_instant(checkpoint)
    )
}

pub(super) fn range_pages(
    requested: InspectionRange,
    aggregate_day_floor: Option<i64>,
    completed_as_of_ms: Option<i64>,
    read_at: i64,
    buckets: Vec<codex_state::accounting::InspectionBucket>,
) -> Vec<InspectorPage> {
    let mut context = vec![
        format!(
            "Requested: {}; timezone: UTC; grouping: {:?}",
            interval(requested.start_ms, requested.end_ms),
            requested.grouping
        ),
        format!(
            "Retention: request detail kept since {}; daily totals {}",
            read_at
                .checked_sub(90 * 86_400_000)
                .filter(|v| *v >= 0)
                .map_or_else(|| "the start".to_string(), utc_instant),
            aggregate_day_floor.map_or_else(
                || "kept: not known yet".to_string(),
                |day| format!("kept since {}", utc_date(day))
            )
        ),
        "Collection coverage: unknown. Range estimate covers root and resolved descendants; unknown ancestry stays separate in bucket breakdowns.".into(),
    ];
    let progress: Vec<_> = buckets
        .iter()
        .map(|b| progress(requested, read_at, completed_as_of_ms, b))
        .collect();
    // Proven post-checkpoint days and buckets not started say nothing about
    // missing detail within the recorded population.
    let states = buckets
        .iter()
        .zip(&progress)
        .flat_map(|(b, progress)| {
            b.days.iter().filter(move |d| match progress {
                None => true,
                Some(BucketProgress::InProgress(_) | BucketProgress::RecordedThrough(_)) => {
                    !matches!(d, InspectionDay::CheckpointLag)
                }
                Some(BucketProgress::NotStarted) => false,
            })
        })
        .collect::<Vec<_>>();
    let complete = buckets.iter().zip(&progress).all(|(b, progress)| {
        progress.is_some()
            || (!b.partial && b.days.iter().all(|d| matches!(d, InspectionDay::Ready(_))))
    });
    // Every day shares one checkpoint. A verified checkpoint-only tail names
    // the recorded cutoff instead of implying missing recorded requests.
    let checkpoint_limited = progress.iter().any(|p| {
        matches!(
            p,
            Some(BucketProgress::InProgress(_) | BucketProgress::RecordedThrough(_))
        )
    });
    if states
        .iter()
        .any(|s| matches!(s, InspectionDay::NeedsRefresh))
    {
        context.push("Recorded totals unavailable — stored contributions need refresh".into());
    } else if states.iter().any(|s| match s {
        InspectionDay::CheckpointLag => true,
        InspectionDay::Ready(v) => {
            v.read_at_ms > v.coverage.completed_as_of_ms && !checkpoint_limited
        }
        InspectionDay::DetailUnavailable {
            coverage,
            read_at_ms,
            ..
        } => *read_at_ms > coverage.completed_as_of_ms && !checkpoint_limited,
        _ => false,
    }) {
        context.push(NOT_CURRENT.into());
    }
    let (unknown_attempts, unavailable_entries) = states
        .iter()
        .filter_map(|state| match state {
            InspectionDay::Ready(view) => Some(view),
            _ => None,
        })
        .fold((0, 0), |(attempts, unavailable), view| {
            (
                attempts + view.unknown_parent_totals.attempts,
                unavailable + view.unknown_parent_unavailable_threads,
            )
        });
    context.push(format!(
        "Range: Unknown parent population: {unknown_attempts} inspectable attempts, excluded from range total"
    ));
    if unavailable_entries > 0 {
        context.push(format!(
            "Range: Unresolved ancestry: {unavailable_entries} thread-slice entries have unavailable detail; their costs and retention coverage are unknown and excluded from this root."
        ));
    }
    if states.iter().any(|s| !matches!(s, InspectionDay::Ready(_))) {
        context.push("Ancestry counts cover inspectable slices only; unavailable slices may contain additional unknown ancestry.".into());
    }
    let mut pages = vec![InspectorPage {
        title: "Recorded request range".into(),
        text: context.clone(),
        links: vec![],
        parent: None,
        selected: Arc::default(),
    }];
    // #314 (D228): a day in the range on which a request went unrecorded.
    if states
        .iter()
        .any(|state| matches!(state, InspectionDay::Ready(view) if view.recording_gap))
    {
        pages[0].text.push(recording_gap_line("this range"));
    }
    pages[0].text.extend(scope::other_conversations_lines(
        /*others*/ None, /*named*/ false,
    ));
    if complete {
        if let Some(end) = progress
            .iter()
            .filter_map(|p| match p {
                Some(BucketProgress::InProgress(end)) => Some(*end),
                Some(BucketProgress::RecordedThrough(_))
                | Some(BucketProgress::NotStarted)
                | None => None,
            })
            .max()
        {
            pages[0].text.push(in_progress_line(end));
        } else if let Some(checkpoint) = progress.iter().find_map(|p| match p {
            Some(BucketProgress::RecordedThrough(checkpoint)) => Some(*checkpoint),
            Some(BucketProgress::InProgress(_)) | Some(BucketProgress::NotStarted) | None => None,
        }) {
            pages[0].text.push(recorded_through_line(checkpoint));
        }
        let quotes = buckets
            .iter()
            .flat_map(|b| &b.days)
            .filter_map(|d| match d {
                InspectionDay::Ready(v) => Some(v),
                _ => None,
            })
            .flat_map(|v| v.requests.values().flatten());
        let mut named = unpriced_rows(quotes.clone(), names_accounts(quotes.clone()));
        named.extend(scope::no_price_next_step(quotes.clone()));
        named.extend(billed_line(&quotes.clone().collect::<Vec<_>>()));
        named.extend(scope::overflow_note(quotes.clone()));
        let gaps = EstimateGaps::of(quotes.clone());
        match codex_state::accounting::DayTotals::from_quotes(quotes) {
            Ok(total) => {
                pages[0].text.extend(estimate(&total, gaps));
                pages[0].text.extend(named);
            }
            // A total this client cannot compute does not unsay which attempts
            // went unpriced; that is a separate statement about the same rows.
            Err(_) => {
                pages[0]
                    .text
                    .push("Range total unavailable — arithmetic overflow".into());
                pages[0].text.extend(named);
            }
        }
    } else {
        // #1009: the not-current notice is left out for a checkpoint-limited
        // range, so the overview names the cutoff here too.
        if let Some(checkpoint) = progress
            .iter()
            .filter_map(|p| match p {
                Some(BucketProgress::InProgress(end)) => Some(*end - 1),
                Some(BucketProgress::RecordedThrough(checkpoint)) => Some(*checkpoint),
                Some(BucketProgress::NotStarted) | None => None,
            })
            .max()
        {
            pages[0].text.push(recorded_through_line(checkpoint));
        }
        pages[0].text.push(
            "Range total unavailable — partial or unavailable buckets excluded; no partial total."
                .into(),
        );
    }
    for (bucket, progress) in buckets.into_iter().zip(progress) {
        let bounds = interval(bucket.start_ms, bucket.end_ms);
        let effective = match (bucket.effective, progress) {
            (Some((s, e)), _) => interval(s, e),
            (None, Some(BucketProgress::NotStarted)) => "not started".into(),
            (None, Some(BucketProgress::RecordedThrough(_))) => {
                "no recorded interval through the ledger checkpoint".into()
            }
            (None, _) => "unavailable".into(),
        };
        let mut header = context.clone();
        header.push(format!(
            "Bucket: {bounds}; effective coverage (requested ∩ aggregate retention ∩ snapshot): {effective}"
        ));
        header.push(match progress {
            Some(BucketProgress::InProgress(end)) => in_progress_line(end),
            Some(BucketProgress::RecordedThrough(checkpoint)) => recorded_through_line(checkpoint),
            Some(BucketProgress::NotStarted) => NOT_STARTED.into(),
            None if bucket.partial => "Partial bucket — excluded from totals".into(),
            None => "Whole bucket within aggregate retention coverage; detail availability checked separately".into(),
        });
        pages[0].text.push(format!(
            "Effective coverage (requested ∩ aggregate retention ∩ snapshot) for {bounds}: {effective}"
        ));
        let mut merged: Option<codex_state::accounting::Inspection> = None;
        let mut diagnostics = Vec::new();
        let mut detail_unavailable = false;
        let mut skipped = false;
        for day in bucket.days {
            match day {
                InspectionDay::Ready(view) => {
                    if let Some(target) = &mut merged {
                        for (id, quotes) in view.requests {
                            target.requests.entry(id).or_default().extend(quotes);
                        }
                        for (id, quotes) in view.unknown_parent_requests {
                            target
                                .unknown_parent_requests
                                .entry(id)
                                .or_default()
                                .extend(quotes);
                        }
                        target.unknown_parent_unavailable_threads +=
                            view.unknown_parent_unavailable_threads;
                        // #314 (D228): any day's gap leaves the bucket incomplete.
                        target.recording_gap |= view.recording_gap;
                        // #1196: days classified differently prove only
                        // that nothing was recorded.
                        target.empty_day = match (target.empty_day, view.empty_day) {
                            (Some(first), Some(next)) if first == next => Some(first),
                            (Some(_), Some(_)) => Some(EmptyDay::NotRecorded),
                            _ => None,
                        };
                    } else {
                        merged = Some(view);
                    }
                }
                // Days not reached yet have nothing to report.
                InspectionDay::CheckpointLag if progress.is_some() => skipped = true,
                other => {
                    detail_unavailable |= matches!(other, InspectionDay::DetailUnavailable { .. });
                    diagnostics.extend(inspection_pages(Ok(other)).remove(0).text);
                }
            }
        }
        // A claim about days holds for the bucket only if the whole bucket,
        // days skipped above included, ends before what the claim dates. A
        // skipped day was never checked for threads of unknown ancestry, so
        // it leaves the conversation's start unproven.
        if let Some(view) = &mut merged {
            let ends_by = |day: i64| bucket.end_ms <= day.saturating_mul(86_400_000);
            view.empty_day = view.empty_day.map(|empty| match empty {
                EmptyDay::BeforeConversation { started_day }
                    if !skipped && ends_by(started_day) =>
                {
                    empty
                }
                EmptyDay::BeforeLedger { ledger_started_day } if ends_by(ledger_started_day) => {
                    empty
                }
                _ => EmptyDay::NotRecorded,
            });
        }
        let available = diagnostics.is_empty();
        header.extend(diagnostics);
        if detail_unavailable {
            header.push(format!("Precision unsupported outside retained raw detail; compacted days lost request/provider attribution. No bucket total. Whole UTC-day bounds offered: {}; this does not restore attribution.", interval(requested.start_ms / 86_400_000 * 86_400_000, ((requested.end_ms - 1) / 86_400_000 + 1) * 86_400_000)));
        }
        let offset = pages.len();
        pages[0].links.push((
            format!(
                "{:?} {bounds}{}",
                requested.grouping,
                match progress {
                    Some(BucketProgress::InProgress(_)) => " (in progress)",
                    Some(BucketProgress::RecordedThrough(_)) => " (recorded through checkpoint)",
                    Some(BucketProgress::NotStarted) => " (not started)",
                    None => "",
                }
            ),
            offset,
        ));
        let mut children = if matches!(progress, Some(BucketProgress::NotStarted)) {
            vec![InspectorPage {
                title: "Not started yet".into(),
                text: vec![],
                links: vec![],
                parent: None,
                selected: Arc::default(),
            }]
        } else if matches!(progress, Some(BucketProgress::RecordedThrough(checkpoint)) if checkpoint < bucket.start_ms)
        {
            vec![InspectorPage {
                title: "Recorded requests through checkpoint".into(),
                text: vec![
                    "Nothing recorded yet: this bucket starts after the ledger checkpoint.".into(),
                ],
                links: vec![],
                parent: None,
                selected: Arc::default(),
            }]
        } else if (!bucket.partial || progress.is_some()) && available {
            if let Some(mut view) = merged {
                let recalculate = (|| -> anyhow::Result<()> {
                    view.totals = codex_state::accounting::DayTotals::from_quotes(
                        view.requests.values().flatten(),
                    )?;
                    view.own_totals = codex_state::accounting::DayTotals::from_quotes(
                        view.requests
                            .values()
                            .flatten()
                            .filter(|q| q.attempt.thread_id == view.owner),
                    )?;
                    view.descendant_totals = codex_state::accounting::DayTotals::from_quotes(
                        view.requests
                            .values()
                            .flatten()
                            .filter(|q| q.attempt.thread_id != view.owner),
                    )?;
                    view.unknown_parent_totals = codex_state::accounting::DayTotals::from_quotes(
                        view.unknown_parent_requests.values().flatten(),
                    )?;
                    Ok(())
                })();
                match recalculate {
                    // The merged view keeps its first day's `utc_day`;
                    // the heading names the whole bucket instead.
                    Ok(()) if progress.is_some() => {
                        let period = if matches!(progress, Some(BucketProgress::InProgress(_))) {
                            format!("so far in {bounds}")
                        } else {
                            bounds.clone()
                        };
                        let mut pages =
                            inspection_pages_for(Ok(InspectionDay::Ready(view)), Some(period));
                        if matches!(progress, Some(BucketProgress::InProgress(_))) {
                            pages[0].title = "Cost so far — this conversation".into();
                        }
                        for page in &mut pages {
                            page.text.retain(|line| line != NOT_CURRENT);
                        }
                        pages
                    }
                    Ok(()) => {
                        inspection_pages_for(Ok(InspectionDay::Ready(view)), Some(bounds.clone()))
                    }
                    Err(_) => inspection_pages(Err(
                        "Bucket total unavailable — arithmetic overflow".into(),
                    )),
                }
            } else {
                inspection_pages(Err("Effective detail unavailable".into()))
            }
        } else {
            vec![InspectorPage {
                title: "Bucket unavailable".into(),
                text: vec![],
                links: vec![],
                parent: None,
                selected: Arc::default(),
            }]
        };
        for child in &mut children {
            child.text.retain(|t| !t.starts_with(DAY_COVERED));
            child.text.splice(0..0, header.clone());
            for text in &mut child.text {
                *text = text
                    .replace(
                        "this UTC day",
                        if requested.grouping == InspectionGrouping::Hour {
                            "this UTC hour"
                        } else {
                            "this bucket"
                        },
                    )
                    .replace("attempts on other days", "attempts outside this bucket")
                    .replace(
                        "after this day",
                        if requested.grouping == InspectionGrouping::Hour {
                            "after this UTC hour"
                        } else {
                            "after this bucket"
                        },
                    )
                    .replace(
                        "anything on this day.",
                        if requested.grouping == InspectionGrouping::Hour {
                            "anything on this hour's UTC day."
                        } else {
                            "anything on this bucket's UTC days."
                        },
                    )
                    .replace(
                        "threads have unavailable day detail",
                        "thread-day entries have unavailable detail",
                    );
                if text.starts_with("Unknown parent population:")
                    || text.starts_with("Unresolved ancestry:")
                {
                    *text = format!("Bucket: {text}");
                }
            }
            child.parent = Some(child.parent.map_or(0, |p| p + offset));
            for (_, target) in &mut child.links {
                *target += offset;
            }
        }
        pages.extend(children);
    }
    pages
}
