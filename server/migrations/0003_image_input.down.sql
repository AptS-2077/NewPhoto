DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM tasks WHERE request->>'mode'='i2i')
        OR EXISTS (SELECT 1 FROM assets WHERE kind='reference') THEN
        RAISE EXCEPTION 'Restore a pre-upgrade backup to roll back after new image-to-image data is created';
    END IF;
END $$;

DROP TABLE task_reference_assets;
DROP INDEX reference_assets_by_owner;
ALTER TABLE assets DROP CONSTRAINT assets_kind_check;
ALTER TABLE assets ADD CONSTRAINT assets_kind_check CHECK (kind IN ('input','result'));
ALTER TABLE tasks DROP COLUMN usage;
ALTER TABLE tasks DROP COLUMN "references";
ALTER TABLE tasks DROP COLUMN mode;
DELETE FROM app_settings WHERE key='image_input_settings';
