CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    agent_name TEXT,
    working_dir TEXT NOT NULL,
    config_snapshot TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    status TEXT NOT NULL DEFAULT 'active'
);

CREATE TABLE IF NOT EXISTS steps (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    step_number INTEGER NOT NULL,
    action_type TEXT NOT NULL,
    action_detail TEXT NOT NULL,
    reason TEXT,
    verification_result TEXT NOT NULL,
    verification_detail TEXT,
    checkpoint_id TEXT,
    result TEXT NOT NULL,
    result_detail TEXT,
    tokens_used INTEGER DEFAULT 0,
    cost_usd REAL DEFAULT 0.0,
    duration_ms INTEGER DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_steps_session ON steps(session_id, step_number);

CREATE TABLE IF NOT EXISTS checkpoints (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES sessions(id),
    step_id TEXT NOT NULL REFERENCES steps(id),
    checkpoint_type TEXT NOT NULL,
    reference TEXT NOT NULL,
    files_affected TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    restored_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_checkpoints_session ON checkpoints(session_id);

CREATE TABLE IF NOT EXISTS anomalies (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    step_id TEXT,
    anomaly_type TEXT NOT NULL,
    severity TEXT NOT NULL,
    detail TEXT NOT NULL,
    detected_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_anomalies_session ON anomalies(session_id);
