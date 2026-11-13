-- One row per model call of a mission's sessions, from the runtime's own log, so the mission's use can be
-- shown call by call. usage_id is the runtime's id for the call, so reading the log again adds nothing twice
-- and a call stays after the runtime's log is compacted. session_key ties the call to a session (and so a slot).
-- at is when the call was recorded (RFC 3339). Counts only: no prompt or reply text is read or stored.
-- Foreign keys are RESTRICT, as elsewhere: deleting a mission deletes its calls explicitly first.

CREATE TABLE mission_usage_call (
    mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE RESTRICT,
    usage_id TEXT NOT NULL,
    runtime TEXT NOT NULL,
    session_key TEXT NOT NULL,
    at TEXT NOT NULL,
    input_tokens INTEGER NOT NULL,
    output_tokens INTEGER NOT NULL,
    cached_tokens INTEGER NOT NULL,
    reasoning_tokens INTEGER NOT NULL,
    PRIMARY KEY (mission_id, usage_id)
);
