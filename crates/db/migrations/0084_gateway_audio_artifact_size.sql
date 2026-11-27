-- The connector validates WAVs up to 8 MiB before artifact publication.
ALTER TABLE public.gateway_artifacts
    DROP CONSTRAINT gateway_artifacts_media_size_check,
    ADD CONSTRAINT gateway_artifacts_media_size_check CHECK (
        (content_type = 'image/png' AND byte_length BETWEEN 1 AND 2097152)
        OR (content_type IN ('audio/wav', 'video/mp4', 'model/gltf-binary') AND byte_length BETWEEN 1 AND 8388608)
    );

INSERT INTO public.gateway_schema_compatibility (migration_version, oldest_gateway_schema) VALUES (84, 83);
