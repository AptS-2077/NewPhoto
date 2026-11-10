CREATE TABLE users (
    id uuid PRIMARY KEY, username text NOT NULL UNIQUE, display_name text NOT NULL,
    role text NOT NULL CHECK (role IN ('admin','user')),
    status text NOT NULL DEFAULT 'enabled' CHECK (status IN ('enabled','disabled')),
    permissions jsonb NOT NULL DEFAULT '[]',
    model_access text NOT NULL DEFAULT 'selected' CHECK (model_access IN ('selected','all_enabled')),
    revision bigint NOT NULL DEFAULT 1, created_at timestamptz NOT NULL DEFAULT now(),
    last_login_at timestamptz, deleted_at timestamptz
);
CREATE TABLE auth_identities (
    id uuid PRIMARY KEY, user_id uuid NOT NULL REFERENCES users(id), provider_id text NOT NULL,
    subject text NOT NULL, UNIQUE(provider_id, subject)
);
CREATE TABLE local_credentials (identity_id uuid PRIMARY KEY REFERENCES auth_identities(id), password_hash text NOT NULL);
CREATE TABLE sessions (
    token_hash text PRIMARY KEY, user_id uuid NOT NULL REFERENCES users(id), csrf_token text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(), expires_at timestamptz NOT NULL DEFAULT now() + interval '8 hours',
    last_seen_at timestamptz NOT NULL DEFAULT now(), revoked_at timestamptz
);
CREATE INDEX sessions_user ON sessions(user_id);
CREATE TABLE login_limits (bucket text PRIMARY KEY, attempts integer NOT NULL, window_start timestamptz NOT NULL DEFAULT now());
CREATE TABLE model_configs (
    id uuid PRIMARY KEY, display_name text NOT NULL, entry_format_id text NOT NULL, upstream_model text NOT NULL,
    api_base_url text NOT NULL, credential_cipher text, credential_epoch bigint NOT NULL DEFAULT 1,
    capabilities jsonb NOT NULL, defaults jsonb NOT NULL DEFAULT '{}', enabled boolean NOT NULL DEFAULT false,
    revision bigint NOT NULL DEFAULT 1, created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(), deleted_at timestamptz
);
CREATE TABLE user_model_grants (
    user_id uuid NOT NULL REFERENCES users(id), model_id uuid NOT NULL REFERENCES model_configs(id),
    PRIMARY KEY(user_id, model_id)
);
CREATE TABLE plugin_packages (
    id uuid PRIMARY KEY, plugin_id text NOT NULL, version text NOT NULL, digest text NOT NULL,
    manifest jsonb NOT NULL, entries jsonb NOT NULL, config_schema jsonb NOT NULL,
    package_key text NOT NULL, created_at timestamptz NOT NULL DEFAULT now(), UNIQUE(plugin_id, version)
);
CREATE TABLE plugin_installations (
    id uuid PRIMARY KEY, package_id uuid NOT NULL REFERENCES plugin_packages(id), alias text NOT NULL DEFAULT '',
    notes text NOT NULL DEFAULT '', config jsonb NOT NULL DEFAULT '{}', desired_enabled boolean NOT NULL DEFAULT true,
    active_enabled boolean NOT NULL DEFAULT false, state text NOT NULL DEFAULT 'installed_pending',
    revision bigint NOT NULL DEFAULT 1, published_revision bigint, created_at timestamptz NOT NULL DEFAULT now(),
    removed_at timestamptz, UNIQUE(package_id)
);
CREATE TABLE registry_revisions (
    revision bigint PRIMARY KEY, snapshot jsonb NOT NULL, request_id text NOT NULL UNIQUE,
    request_hash text NOT NULL, created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE tasks (
    id uuid PRIMARY KEY, user_id uuid NOT NULL REFERENCES users(id), idempotency_key text NOT NULL,
    request_hash text NOT NULL, request jsonb NOT NULL, model_id uuid NOT NULL REFERENCES model_configs(id),
    model_snapshot jsonb NOT NULL, entry_snapshot jsonb NOT NULL, registry_revision bigint NOT NULL,
    status text NOT NULL CHECK (status IN ('queued','preparing','submitting','saving','succeeded','failed','submission_unknown','result_unavailable')),
    message text NOT NULL DEFAULT '', created_at timestamptz NOT NULL DEFAULT now(), started_at timestamptz,
    ended_at timestamptz, queue_ms bigint, execution_ms bigint, total_ms bigint,
    lease_until timestamptz, lease_token uuid, retry_count integer NOT NULL DEFAULT 0,
    parent_task_id uuid REFERENCES tasks(id), UNIQUE(user_id, idempotency_key)
);
CREATE INDEX tasks_queue ON tasks(status, created_at);
CREATE INDEX tasks_owner ON tasks(user_id, created_at DESC);
CREATE TABLE task_attempts (
    id uuid PRIMARY KEY, task_id uuid NOT NULL UNIQUE REFERENCES tasks(id),
    request_may_have_been_sent boolean NOT NULL DEFAULT false, response_complete boolean NOT NULL DEFAULT false,
    receipt_key text, response_content_type text, submission_status integer, created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE upstream_requests (
    id uuid PRIMARY KEY, task_id uuid NOT NULL REFERENCES tasks(id), phase text NOT NULL,
    started_at timestamptz NOT NULL DEFAULT now(), headers_at timestamptz, ended_at timestamptz,
    duration_ms bigint, http_status integer, transport_error text, summary text NOT NULL DEFAULT '', upstream_request_id text
);
CREATE TABLE assets (
    id uuid PRIMARY KEY, user_id uuid NOT NULL REFERENCES users(id), task_id uuid REFERENCES tasks(id),
    kind text NOT NULL CHECK (kind IN ('input','result')), object_key text NOT NULL UNIQUE, thumbnail_key text NOT NULL,
    digest text NOT NULL, mime text NOT NULL, width integer NOT NULL, height integer NOT NULL,
    byte_size bigint NOT NULL, created_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX one_result_per_task ON assets(task_id) WHERE kind = 'result';
CREATE TABLE audit_logs (
    id uuid PRIMARY KEY, actor_id uuid REFERENCES users(id), target text NOT NULL, category text NOT NULL,
    detail jsonb NOT NULL DEFAULT '{}', created_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE app_settings (key text PRIMARY KEY, value jsonb NOT NULL);
INSERT INTO app_settings VALUES ('generation_paused', 'false');
