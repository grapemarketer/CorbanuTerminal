CREATE TABLE draft_accounting_gap_days (
    thread_id TEXT NOT NULL, utc_day INTEGER NOT NULL CHECK(utc_day >= 0),
    PRIMARY KEY(thread_id, utc_day));
CREATE TABLE draft_accounting_ledger_format_7 (
    version INTEGER PRIMARY KEY NOT NULL CHECK(version = 7));
INSERT INTO draft_accounting_ledger_format_7 VALUES (7);
-- Ledger format 7: a UTC day on which accounting failed to record at least
-- one of a conversation's model requests (#314, D228). The request was sent
-- and billed but has no record, so that day's totals may be incomplete. Only
-- a home where such a failure happened is upgraded; a build that knows only
-- formats 1 to 6 refuses this ledger instead of showing those totals as
-- complete.
