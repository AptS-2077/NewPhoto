ALTER TABLE tasks ADD COLUMN error_code text;
UPDATE tasks SET error_code = CASE status WHEN 'submission_unknown' THEN 'SUBMISSION_UNKNOWN' WHEN 'result_unavailable' THEN 'RESULT_UNAVAILABLE' WHEN 'failed' THEN 'TASK_FAILED' END;
