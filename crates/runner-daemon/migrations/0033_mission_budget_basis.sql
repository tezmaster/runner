-- What a mission, and everything else on the machine, had used of a runtime's tokens in a weekly allowance
-- window at the time of a reading. A mission's share of the allowance is the window's points used times its
-- share of the tokens, so both are kept; the four parts are stored apart because whether cached input counts
-- against an allowance is not known, and the weighting can then change without reading the logs again.
-- week_start is when the window began (its reset time minus seven days). mission_* is the mission's own sessions;
-- machine_* is every session of that runtime on this machine, including this mission's and any begun outside
-- Runner. Counts are cumulative from week_start to read_at. Foreign keys are RESTRICT, as elsewhere.

CREATE TABLE mission_budget_basis (
    mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE RESTRICT,
    runtime TEXT NOT NULL,
    read_at TEXT NOT NULL,
    week_start TEXT NOT NULL,
    mission_input INTEGER NOT NULL,
    mission_output INTEGER NOT NULL,
    mission_cached INTEGER NOT NULL,
    mission_reasoning INTEGER NOT NULL,
    machine_input INTEGER NOT NULL,
    machine_output INTEGER NOT NULL,
    machine_cached INTEGER NOT NULL,
    machine_reasoning INTEGER NOT NULL,
    PRIMARY KEY (mission_id, runtime, read_at)
);
