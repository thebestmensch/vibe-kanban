-- JM-717: persist aggregated CI-check status for open pull requests.
-- NULL = unknown/unavailable (not yet polled, or a non-GitHub provider);
-- non-null values are 'passing' | 'failing' | 'pending' | 'no_checks'.
ALTER TABLE pull_requests ADD COLUMN check_status TEXT;
