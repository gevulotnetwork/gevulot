
-- Drop unique (parent, prover) constraint. Multiple nodes will generate proof
-- for same Run transaction, using same prover program so this constraint is
-- wrong.
ALTER TABLE proof DROP CONSTRAINT proof_parent_prover_key;
