-- Readings of a runtime's plan allowance taken while a mission runs, so a mission can be shown as a share of
-- the allowance it used. Allowances are account-wide: a reading includes anything else on the account, which
-- the app says next to every figure. window_name is the runtime's own name for the window ('Week', '5 hours').
-- used_percent is the runtime's whole-number percent used. resets_at is when the window resets (NULL when the
-- runtime does not say). read_at is when the runtime was asked. kind is why: 'start' (the mission started),
-- 'join' (the runtime joined later, or was first read later), 'view' (someone looked while it ran) or 'stop'.
-- Foreign keys are RESTRICT, as elsewhere: deleting a mission deletes its readings explicitly first.

CREATE TABLE mission_budget_sample (
    mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE RESTRICT,
    runtime TEXT NOT NULL,
    window_name TEXT NOT NULL,
    used_percent REAL NOT NULL,
    resets_at TEXT,
    read_at TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('start', 'join', 'view', 'stop')),
    PRIMARY KEY (mission_id, runtime, window_name, read_at)
);
