ALTER TABLE tasks ADD COLUMN mode text NOT NULL DEFAULT 't2i' CHECK (mode IN ('t2i','i2i'));
ALTER TABLE tasks ADD COLUMN "references" jsonb NOT NULL DEFAULT '[]'::jsonb CHECK (jsonb_typeof("references") = 'array' AND jsonb_array_length("references") <= 4);
ALTER TABLE tasks ADD COLUMN usage jsonb;

ALTER TABLE assets DROP CONSTRAINT assets_kind_check;
ALTER TABLE assets ADD CONSTRAINT assets_kind_check CHECK (kind IN ('input','result','reference'));
CREATE INDEX reference_assets_by_owner ON assets(user_id,created_at) WHERE kind='reference';

CREATE TABLE task_reference_assets (
    task_id uuid NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    asset_id uuid NOT NULL REFERENCES assets(id),
    position smallint NOT NULL CHECK (position BETWEEN 0 AND 3),
    weight double precision CHECK (weight >= 0 AND weight <= 1),
    PRIMARY KEY (task_id, position),
    UNIQUE (task_id, asset_id)
);
CREATE INDEX task_reference_assets_asset ON task_reference_assets(asset_id);

UPDATE tasks
SET mode='i2i',
    "references"=jsonb_build_array(jsonb_build_object('id',request->'input_asset_ids'->>0))
WHERE entry_snapshot->>'operation'='edit' AND request->'input_asset_ids'->>0 IS NOT NULL;

INSERT INTO task_reference_assets(task_id,asset_id,position,weight)
SELECT t.id, a.id, 0, NULL
FROM tasks t
JOIN assets a ON a.id=(t.request->'input_asset_ids'->>0)::uuid AND a.user_id=t.user_id AND a.kind='input'
WHERE t.mode='i2i';

INSERT INTO app_settings(key,value) VALUES
    ('image_input_settings','{"enabled":true,"max_bytes":10485760,"revision":1}'::jsonb)
ON CONFLICT (key) DO NOTHING;
