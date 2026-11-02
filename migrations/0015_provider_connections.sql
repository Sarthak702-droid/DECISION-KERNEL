-- Connections are independent of immutable packs. Empty project means a tenant
-- connection, available to projects in that tenant/environment only.
CREATE TABLE provider_connections (
 tenant_id text NOT NULL, project_id text NOT NULL, environment text NOT NULL,
 connection_id text NOT NULL, config jsonb NOT NULL,
 encrypted_key bytea, revision bigint NOT NULL DEFAULT 1,
 deleted boolean NOT NULL DEFAULT false,
 updated_at timestamptz NOT NULL DEFAULT now(),
 PRIMARY KEY (tenant_id,project_id,environment,connection_id),
 CHECK (deleted = (encrypted_key IS NULL))
);
CREATE TABLE provider_bindings (
 tenant_id text NOT NULL, project_id text NOT NULL, environment text NOT NULL,
 product_id text NOT NULL DEFAULT current_setting('kernel.product_id',true),
 decision_type text NOT NULL, connection_project_id text NOT NULL,
 connection_id text NOT NULL,
 PRIMARY KEY (tenant_id,project_id,environment,product_id,decision_type),
 FOREIGN KEY (tenant_id,connection_project_id,environment,connection_id)
 REFERENCES provider_connections (tenant_id,project_id,environment,connection_id),
 CHECK (connection_project_id = '' OR connection_project_id = project_id)
);
ALTER TABLE provider_connections ENABLE ROW LEVEL SECURITY;
ALTER TABLE provider_connections FORCE ROW LEVEL SECURITY;
CREATE POLICY kernel_scope ON provider_connections TO kernel_runtime
 USING (tenant_id=current_setting('kernel.tenant_id',true)
 AND environment=current_setting('kernel.environment',true)
 AND (project_id='' OR project_id=current_setting('kernel.project_id',true)))
 WITH CHECK (tenant_id=current_setting('kernel.tenant_id',true)
 AND environment=current_setting('kernel.environment',true)
 AND (project_id='' OR project_id=current_setting('kernel.project_id',true)));
ALTER TABLE provider_bindings ENABLE ROW LEVEL SECURITY;
ALTER TABLE provider_bindings FORCE ROW LEVEL SECURITY;
CREATE POLICY kernel_scope ON provider_bindings TO kernel_runtime
 USING (tenant_id=current_setting('kernel.tenant_id',true)
 AND project_id=current_setting('kernel.project_id',true)
 AND environment=current_setting('kernel.environment',true)
 AND product_id=current_setting('kernel.product_id',true))
 WITH CHECK (tenant_id=current_setting('kernel.tenant_id',true)
 AND project_id=current_setting('kernel.project_id',true)
 AND environment=current_setting('kernel.environment',true)
 AND product_id=current_setting('kernel.product_id',true));
GRANT SELECT, INSERT, UPDATE ON provider_connections TO kernel_runtime;
GRANT SELECT, INSERT, UPDATE, DELETE ON provider_bindings TO kernel_runtime;
