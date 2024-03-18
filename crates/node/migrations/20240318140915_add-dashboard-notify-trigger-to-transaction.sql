
-- Following function is triggered on new rows on `transaction` table.
-- It generates a JSON entity that can be sent to devnet dashboard UI
-- that streams transactions from the devnet.

CREATE OR REPLACE FUNCTION dashboard_data_row() RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
	run_tx				VARCHAR(64);
	prover_id  			VARCHAR(64);
	prover_tag 			VARCHAR(128);
	payload				TEXT;
	proof_count 		INTEGER;
	verification_count  INTEGER;
BEGIN
	IF NEW.kind = 'run' THEN
		-- Handle run tx.
		SELECT program INTO prover_id FROM workflow_step WHERE sequence = 1 AND tx = NEW.hash;
		SELECT name INTO prover_tag FROM program WHERE hash = prover_id;

		payload := FORMAT('{"state":"submitted", "tx_id":"%s", "tag":"%s", "prover_id": "%s", "timestamp":"%s"}', NEW.hash, prover_tag, prover_id, to_char(NEW.created_at, 'YYYY-MM-DD"T"HH24:MI:SS"Z"'));
		PERFORM pg_notify('dashboard_data_stream', payload);
	ELSIF NEW.kind = 'proof' THEN
		-- Handle proof tx.
		SELECT parent INTO run_tx FROM proof WHERE tx = NEW.hash;
		SELECT COUNT(*) INTO proof_count FROM proof WHERE parent = run_tx;
		SELECT prover INTO prover_id FROM proof WHERE tx = NEW.hash;
		SELECT name INTO prover_tag FROM program WHERE hash = prover_id;
		IF proof_count < 2 THEN
			payload := FORMAT('{"state":"proving", "tx_id":"%s", "tag":"%s", "prover_id": "%s", "timestamp":"%s"}', run_tx, prover_tag, prover_id, to_char(NEW.created_at, 'YYYY-MM-DD"T"HH24:MI:SS"Z"'));
		ELSE
			payload := FORMAT('{"state":"verifying", "tx_id":"%s", "tag":"%s", "prover_id": "%s", "timestamp":"%s"}', run_tx, prover_tag, prover_id, to_char(NEW.created_at, 'YYYY-MM-DD"T"HH24:MI:SS"Z"'));
		END IF;
		PERFORM pg_notify('dashboard_data_stream', payload);
	ELSIF NEW.kind = 'verification' THEN
		-- Handle verification tx.
		SELECT p.parent INTO run_tx FROM proof AS p JOIN verification AS v ON p.tx = v.parent WHERE v.tx = NEW.hash;
		SELECT COUNT(*) INTO verification_count FROM verification AS v JOIN proof AS p ON v.parent = p.tx WHERE p.parent = run_tx;
		SELECT prover INTO prover_id FROM proof WHERE parent = run_tx LIMIT 1;
		SELECT name INTO prover_tag FROM program WHERE hash = prover_id;
		IF verification_count < 3 THEN
			payload := FORMAT('{"state":"verifying", "tx_id":"%s", "tag":"%s", "prover_id": "%s", "timestamp":"%s"}', run_tx, prover_tag, prover_id, to_char(NEW.created_at, 'YYYY-MM-DD"T"HH24:MI:SS"Z"'));
		ELSE
			payload := FORMAT('{"state":"complete", "tx_id":"%s", "tag":"%s", "prover_id": "%s", "timestamp":"%s"}', run_tx, prover_tag, prover_id, to_char(NEW.created_at, 'YYYY-MM-DD"T"HH24:MI:SS"Z"'));
		END IF;
		PERFORM pg_notify('dashboard_data_stream', payload);
	END IF;
	RETURN NULL;
END;
$$;

DROP TRIGGER IF EXISTS notify_dashboard ON transaction;
CREATE CONSTRAINT TRIGGER notify_dashboard AFTER INSERT ON transaction DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE PROCEDURE public.dashboard_data_row();
