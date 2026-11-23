PRAGMA user_version = 1;
CREATE TABLE IF NOT EXISTS runs (
    id TEXT PRIMARY KEY,
    prompt_id TEXT NOT NULL UNIQUE,
    client_id TEXT NOT NULL,
    workflow_id TEXT NOT NULL,
    workflow_version TEXT NOT NULL,
    workflow_sha256 TEXT NOT NULL,
    generation_mode TEXT NOT NULL,
    character_id TEXT,
    status TEXT NOT NULL,
    parameters_json TEXT NOT NULL,
    gate_snapshot_json TEXT,
    output_dir TEXT NOT NULL,
    manifest_path TEXT,
    error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    completed_at TEXT
);
INSERT INTO runs VALUES (
    'run-legacy', 'prompt-legacy', 'client-legacy', 'text-to-image', '1.0.0', 'workflow-hash',
    'generic', NULL, 'completed', '{"seed":42}', NULL, 'runs/text-to-image/v1',
    'runs/text-to-image/v1/run-manifest.json', NULL, '2026-01-01T10:00:00+00:00',
    '2026-01-01T10:01:00+00:00', '2026-01-01T10:01:00+00:00'
);
