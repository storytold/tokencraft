-- Run after preparing the import tables, using their owner with database grant privileges.
\set ON_ERROR_STOP on
SELECT current_database() AS destination_database \gset

BEGIN;
GRANT TEMPORARY ON DATABASE :"destination_database" TO :"sync_role";
GRANT USAGE ON SCHEMA tokencraft_export TO :"sync_role";
GRANT SELECT, INSERT ON TABLE tokencraft_export.events TO :"sync_role";
GRANT SELECT, INSERT, UPDATE ON TABLE tokencraft_export.checkpoints TO :"sync_role";
COMMIT;
