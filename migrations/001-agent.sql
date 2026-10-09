CREATE TABLE lenso_agent_schema (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  version INTEGER NOT NULL
);

INSERT INTO lenso_agent_schema (singleton, version) VALUES (1, 1);

CREATE TABLE lenso_agent_sessions (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  id TEXT NOT NULL UNIQUE,
  session_id TEXT NOT NULL,
  value TEXT NOT NULL CHECK (json_valid(value))
);
CREATE INDEX lenso_agent_sessions_session_id ON lenso_agent_sessions (session_id);

CREATE TABLE lenso_agent_messages (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  id TEXT NOT NULL UNIQUE,
  session_id TEXT NOT NULL,
  value TEXT NOT NULL CHECK (json_valid(value))
);
CREATE INDEX lenso_agent_messages_session_id ON lenso_agent_messages (session_id);

CREATE TABLE lenso_agent_runs (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  id TEXT NOT NULL UNIQUE,
  session_id TEXT NOT NULL,
  value TEXT NOT NULL CHECK (json_valid(value))
);
CREATE INDEX lenso_agent_runs_session_id ON lenso_agent_runs (session_id);

CREATE TABLE lenso_agent_calls (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  id TEXT NOT NULL UNIQUE,
  session_id TEXT NOT NULL,
  value TEXT NOT NULL CHECK (json_valid(value))
);
CREATE INDEX lenso_agent_calls_session_id ON lenso_agent_calls (session_id);

CREATE TABLE lenso_agent_actions (
  sequence INTEGER PRIMARY KEY AUTOINCREMENT,
  id TEXT NOT NULL UNIQUE,
  session_id TEXT NOT NULL,
  value TEXT NOT NULL CHECK (json_valid(value))
);
CREATE INDEX lenso_agent_actions_session_id ON lenso_agent_actions (session_id);
