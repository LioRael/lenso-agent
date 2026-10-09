ALTER TABLE lenso_agent_sessions ADD COLUMN status TEXT;
ALTER TABLE lenso_agent_sessions ADD COLUMN bytes INTEGER NOT NULL DEFAULT 0 CHECK (bytes >= 0);
ALTER TABLE lenso_agent_sessions ADD COLUMN message_value_bytes INTEGER NOT NULL DEFAULT 0 CHECK (message_value_bytes >= 0);
CREATE INDEX lenso_agent_sessions_session_sequence ON lenso_agent_sessions (session_id, sequence, bytes);
CREATE INDEX lenso_agent_sessions_status_session ON lenso_agent_sessions (status, session_id);

ALTER TABLE lenso_agent_messages ADD COLUMN status TEXT;
ALTER TABLE lenso_agent_messages ADD COLUMN bytes INTEGER NOT NULL DEFAULT 0 CHECK (bytes >= 0);
ALTER TABLE lenso_agent_messages ADD COLUMN message_value_bytes INTEGER NOT NULL DEFAULT 0 CHECK (message_value_bytes >= 0);
CREATE INDEX lenso_agent_messages_session_sequence ON lenso_agent_messages (session_id, sequence, bytes);
CREATE INDEX lenso_agent_messages_status_session ON lenso_agent_messages (status, session_id);

ALTER TABLE lenso_agent_runs ADD COLUMN status TEXT;
ALTER TABLE lenso_agent_runs ADD COLUMN bytes INTEGER NOT NULL DEFAULT 0 CHECK (bytes >= 0);
ALTER TABLE lenso_agent_runs ADD COLUMN message_value_bytes INTEGER NOT NULL DEFAULT 0 CHECK (message_value_bytes >= 0);
CREATE INDEX lenso_agent_runs_session_sequence ON lenso_agent_runs (session_id, sequence, bytes);
CREATE INDEX lenso_agent_runs_status_session ON lenso_agent_runs (status, session_id);

ALTER TABLE lenso_agent_calls ADD COLUMN status TEXT;
ALTER TABLE lenso_agent_calls ADD COLUMN bytes INTEGER NOT NULL DEFAULT 0 CHECK (bytes >= 0);
ALTER TABLE lenso_agent_calls ADD COLUMN message_value_bytes INTEGER NOT NULL DEFAULT 0 CHECK (message_value_bytes >= 0);
CREATE INDEX lenso_agent_calls_session_sequence ON lenso_agent_calls (session_id, sequence, bytes);
CREATE INDEX lenso_agent_calls_status_session ON lenso_agent_calls (status, session_id);

ALTER TABLE lenso_agent_actions ADD COLUMN status TEXT;
ALTER TABLE lenso_agent_actions ADD COLUMN bytes INTEGER NOT NULL DEFAULT 0 CHECK (bytes >= 0);
ALTER TABLE lenso_agent_actions ADD COLUMN message_value_bytes INTEGER NOT NULL DEFAULT 0 CHECK (message_value_bytes >= 0);
CREATE INDEX lenso_agent_actions_session_sequence ON lenso_agent_actions (session_id, sequence, bytes);
CREATE INDEX lenso_agent_actions_status_session ON lenso_agent_actions (status, session_id);

UPDATE lenso_agent_schema SET version = 2 WHERE singleton = 1;
