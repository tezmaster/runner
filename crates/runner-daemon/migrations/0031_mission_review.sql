-- A person's verdict on a finished mission, and what each slot used, so runs can be reviewed afterwards.
-- mission_rating: at most one row per mission; rating again replaces it. verdict is 'up' or 'down'; note is
-- free text the person wrote ('' for none) and stays local. Foreign keys are RESTRICT, as elsewhere: deleting a
-- mission deletes its rating and usage rows explicitly first (ops::mission::delete_rows).
-- mission_slot_usage: one row per slot handle, written by whoever harvests the slot's session log (the usage
-- of a runtime is not known to the daemon). Token columns are NULL when the usage could not be read, and
-- not_recorded_reason then says why; NULL is never zero. input_includes_cached is NULL when unknown, because
-- runtimes count cached tokens differently and counts must not be summed across runtimes.

CREATE TABLE mission_rating (
    mission_id TEXT PRIMARY KEY REFERENCES missions(id) ON DELETE RESTRICT,
    verdict TEXT NOT NULL CHECK (verdict IN ('up', 'down')),
    note TEXT NOT NULL DEFAULT '',
    rated_at TEXT NOT NULL
);

CREATE TABLE mission_slot_usage (
    mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE RESTRICT,
    handle TEXT NOT NULL,
    runtime TEXT,
    model TEXT,
    effort TEXT,
    prompt_hash TEXT,
    input_tokens INTEGER,
    output_tokens INTEGER,
    cached_tokens INTEGER,
    reasoning_tokens INTEGER,
    steps INTEGER,
    input_includes_cached INTEGER,
    wall_minutes REAL,
    not_recorded_reason TEXT,
    recorded_at TEXT NOT NULL,
    PRIMARY KEY (mission_id, handle)
);
