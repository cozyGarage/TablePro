use super::*;

#[async_trait]
impl Connection for PolicyGuard {
    /// The guard adds policy, not a driver. Whether an interrupted
    /// operation can be stopped on the server is a property of the
    /// connection underneath, and the interface only ever holds guarded
    /// connections, so failing to forward this would report every engine
    /// as unable to cancel.
    fn supports_server_cancellation(&self) -> bool {
        self.inner.supports_server_cancellation()
    }

    async fn list_tables(&self) -> Result<Vec<TableInfo>, DriverError> {
        let operation = self.metadata_operation("LIST TABLES", Vec::new());
        self.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self.inner.list_tables().await;
        let rows = result.as_ref().ok().map(|tables| tables.len() as u64);
        self.audit_read_result(&operation, start, &result, rows).await?;
        result
    }

    async fn list_tables_controlled(&self, control: &OperationControl) -> Result<Vec<TableInfo>, DriverError> {
        check_pre_dispatch(control)?;
        let operation = self.metadata_operation("LIST TABLES", Vec::new());
        self.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self.inner.list_tables_controlled(control).await;
        let rows = result.as_ref().ok().map(|tables| tables.len() as u64);
        self.audit_controlled_read_result(&operation, start, &result, rows)
            .await?;
        result
    }

    async fn list_views(&self) -> Result<Vec<TableInfo>, DriverError> {
        let operation = self.metadata_operation("LIST VIEWS", Vec::new());
        self.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self.inner.list_views().await;
        let rows = result.as_ref().ok().map(|views| views.len() as u64);
        self.audit_read_result(&operation, start, &result, rows).await?;
        result
    }

    async fn list_views_controlled(&self, control: &OperationControl) -> Result<Vec<TableInfo>, DriverError> {
        check_pre_dispatch(control)?;
        let operation = self.metadata_operation("LIST VIEWS", Vec::new());
        self.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self.inner.list_views_controlled(control).await;
        let rows = result.as_ref().ok().map(|views| views.len() as u64);
        self.audit_controlled_read_result(&operation, start, &result, rows)
            .await?;
        result
    }

    async fn fetch_columns(&self, schema: Option<&str>, table: &str) -> Result<Vec<ColumnInfo>, DriverError> {
        let target = schema.map_or_else(|| table.to_string(), |schema| format!("{schema}.{table}"));
        let operation = self.metadata_operation("FETCH COLUMNS", vec![target]);
        self.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self.inner.fetch_columns(schema, table).await;
        let rows = result.as_ref().ok().map(|columns| columns.len() as u64);
        self.audit_read_result(&operation, start, &result, rows).await?;
        result
    }

    async fn fetch_columns_controlled(
        &self,
        schema: Option<&str>,
        table: &str,
        control: &OperationControl,
    ) -> Result<Vec<ColumnInfo>, DriverError> {
        check_pre_dispatch(control)?;
        let target = schema.map_or_else(|| table.to_string(), |schema| format!("{schema}.{table}"));
        let operation = self.metadata_operation("FETCH COLUMNS", vec![target]);
        self.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self.inner.fetch_columns_controlled(schema, table, control).await;
        let rows = result.as_ref().ok().map(|columns| columns.len() as u64);
        self.audit_controlled_read_result(&operation, start, &result, rows)
            .await?;
        result
    }

    async fn fetch_rows(
        &self,
        schema: Option<&str>,
        table: &str,
        offset: u64,
        limit: u64,
    ) -> Result<QueryResult, DriverError> {
        let target = schema.map_or_else(|| table.to_string(), |schema| format!("{schema}.{table}"));
        let operation = self.metadata_operation("FETCH ROWS", vec![target]);
        self.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self
            .inner
            .fetch_rows(schema, table, offset, limit)
            .await
            .map(|rows| self.mask_result(rows));
        let row_count = result.as_ref().ok().map(|rows| rows.rows.len() as u64);
        self.audit_read_result(&operation, start, &result, row_count).await?;
        result
    }

    async fn fetch_rows_controlled(
        &self,
        schema: Option<&str>,
        table: &str,
        offset: u64,
        limit: u64,
        control: &OperationControl,
    ) -> Result<QueryResult, DriverError> {
        check_pre_dispatch(control)?;
        let target = schema.map_or_else(|| table.to_string(), |schema| format!("{schema}.{table}"));
        let operation = self.metadata_operation("FETCH ROWS", vec![target]);
        self.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self
            .inner
            .fetch_rows_controlled(schema, table, offset, limit, control)
            .await
            .map(|rows| self.mask_result(rows));
        let row_count = result.as_ref().ok().map(|rows| rows.rows.len() as u64);
        self.audit_controlled_read_result(&operation, start, &result, row_count)
            .await?;
        result
    }

    async fn query(&self, sql: &str) -> Result<QueryResult, DriverError> {
        let authorization = self.authorize(sql, false, None).await?;
        if authorization.facts.writes {
            return self
                .execute_query_write(sql, None, authorization, |inner| inner.query(sql))
                .await;
        }
        let operation = self.operation(
            sql,
            None,
            &authorization.facts,
            &authorization.decision,
            authorization.approval_outcome,
            authorization.preview_state,
        );
        self.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self
            .inner
            .query(sql)
            .await
            .map(|value| self.mask_result_for_sql(Some(sql), value));
        let rows = result.as_ref().ok().map(|value| value.rows.len() as u64);
        self.audit_read_result(&operation, start, &result, rows).await?;
        result
    }

    async fn query_controlled(&self, sql: &str, control: &OperationControl) -> Result<QueryResult, DriverError> {
        let authorization = self.authorize(sql, false, Some(control)).await?;
        if authorization.facts.writes {
            return self
                .execute_query_write(sql, None, authorization, |inner| inner.query_controlled(sql, control))
                .await;
        }
        let operation = self.operation(
            sql,
            None,
            &authorization.facts,
            &authorization.decision,
            authorization.approval_outcome,
            authorization.preview_state,
        );
        self.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self
            .inner
            .query_controlled(sql, control)
            .await
            .map(|value| self.mask_result_for_sql(Some(sql), value));
        let rows = result.as_ref().ok().map(|value| value.rows.len() as u64);
        self.audit_controlled_read_result(&operation, start, &result, rows)
            .await?;
        result
    }

    async fn query_params(&self, sql: &str, params: &[Value]) -> Result<QueryResult, DriverError> {
        let authorization = self.authorize(sql, true, None).await?;
        if authorization.facts.writes {
            return self
                .execute_query_write(sql, None, authorization, |inner| inner.query_params(sql, params))
                .await;
        }
        let operation = self.operation(
            sql,
            None,
            &authorization.facts,
            &authorization.decision,
            authorization.approval_outcome,
            authorization.preview_state,
        );
        self.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self
            .inner
            .query_params(sql, params)
            .await
            .map(|value| self.mask_result_for_sql(Some(sql), value));
        let rows = result.as_ref().ok().map(|value| value.rows.len() as u64);
        self.audit_read_result(&operation, start, &result, rows).await?;
        result
    }

    async fn query_params_controlled(
        &self,
        sql: &str,
        params: &[Value],
        control: &OperationControl,
    ) -> Result<QueryResult, DriverError> {
        let authorization = self.authorize(sql, true, Some(control)).await?;
        if authorization.facts.writes {
            return self
                .execute_query_write(sql, None, authorization, |inner| {
                    inner.query_params_controlled(sql, params, control)
                })
                .await;
        }
        let operation = self.operation(
            sql,
            None,
            &authorization.facts,
            &authorization.decision,
            authorization.approval_outcome,
            authorization.preview_state,
        );
        self.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self
            .inner
            .query_params_controlled(sql, params, control)
            .await
            .map(|value| self.mask_result_for_sql(Some(sql), value));
        let rows = result.as_ref().ok().map(|value| value.rows.len() as u64);
        self.audit_controlled_read_result(&operation, start, &result, rows)
            .await?;
        result
    }

    async fn execute(&self, sql: &str) -> Result<ExecResult, DriverError> {
        let authorization = self.authorize(sql, false, None).await?;
        self.execute_write(sql, None, authorization, |inner| inner.execute(sql))
            .await
    }

    async fn execute_controlled(&self, sql: &str, control: &OperationControl) -> Result<ExecResult, DriverError> {
        let authorization = self.authorize(sql, false, Some(control)).await?;
        self.execute_write(sql, None, authorization, |inner| inner.execute_controlled(sql, control))
            .await
    }

    async fn execute_params(&self, sql: &str, params: &[Value]) -> Result<ExecResult, DriverError> {
        let authorization = self.authorize(sql, true, None).await?;
        self.execute_write(sql, None, authorization, |inner| inner.execute_params(sql, params))
            .await
    }

    async fn execute_params_controlled(
        &self,
        sql: &str,
        params: &[Value],
        control: &OperationControl,
    ) -> Result<ExecResult, DriverError> {
        let authorization = self.authorize(sql, true, Some(control)).await?;
        self.execute_write(sql, None, authorization, |inner| {
            inner.execute_params_controlled(sql, params, control)
        })
        .await
    }

    async fn execute_in_transaction(&self, statements: &[(String, Vec<Value>)]) -> Result<Vec<u64>, DriverError> {
        let combined = statements
            .iter()
            .map(|(sql, _)| sql.trim().trim_end_matches(';'))
            .collect::<Vec<_>>()
            .join(";\n");
        let authorization = self.authorize(&combined, true, None).await?;
        self.require_governed_write_available()?;
        let batch_id = Uuid::new_v4();
        let operation = self.operation(
            &combined,
            Some(batch_id),
            &authorization.facts,
            &authorization.decision,
            authorization.approval_outcome,
            authorization.preview_state,
        );
        self.handle_intent_failure(self.record_intent(&operation).await)?;
        let mut pending_write = self.ctx.audit_state.pending_write();
        let start = Instant::now();
        let result = self.inner.execute_in_transaction(statements).await;
        let rows = result.as_ref().ok().map(|values| values.iter().sum());
        self.audit_transaction_result(&operation, start, &result, rows).await?;
        pending_write.disarm();
        result
    }

    async fn fetch_indexes(&self, schema: Option<&str>, table: &str) -> Result<Vec<IndexInfo>, DriverError> {
        let target = schema.map_or_else(|| table.to_string(), |schema| format!("{schema}.{table}"));
        let operation = self.metadata_operation("FETCH INDEXES", vec![target]);
        self.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self.inner.fetch_indexes(schema, table).await;
        let rows = result.as_ref().ok().map(|indexes| indexes.len() as u64);
        self.audit_read_result(&operation, start, &result, rows).await?;
        result
    }

    async fn fetch_indexes_controlled(
        &self,
        schema: Option<&str>,
        table: &str,
        control: &OperationControl,
    ) -> Result<Vec<IndexInfo>, DriverError> {
        check_pre_dispatch(control)?;
        let target = schema.map_or_else(|| table.to_string(), |schema| format!("{schema}.{table}"));
        let operation = self.metadata_operation("FETCH INDEXES", vec![target]);
        self.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self.inner.fetch_indexes_controlled(schema, table, control).await;
        let rows = result.as_ref().ok().map(|indexes| indexes.len() as u64);
        self.audit_controlled_read_result(&operation, start, &result, rows)
            .await?;
        result
    }

    async fn fetch_foreign_keys(&self, schema: Option<&str>, table: &str) -> Result<Vec<ForeignKeyInfo>, DriverError> {
        let target = schema.map_or_else(|| table.to_string(), |schema| format!("{schema}.{table}"));
        let operation = self.metadata_operation("FETCH FOREIGN KEYS", vec![target]);
        self.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self.inner.fetch_foreign_keys(schema, table).await;
        let rows = result.as_ref().ok().map(|keys| keys.len() as u64);
        self.audit_read_result(&operation, start, &result, rows).await?;
        result
    }

    async fn fetch_foreign_keys_controlled(
        &self,
        schema: Option<&str>,
        table: &str,
        control: &OperationControl,
    ) -> Result<Vec<ForeignKeyInfo>, DriverError> {
        check_pre_dispatch(control)?;
        let target = schema.map_or_else(|| table.to_string(), |schema| format!("{schema}.{table}"));
        let operation = self.metadata_operation("FETCH FOREIGN KEYS", vec![target]);
        self.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self.inner.fetch_foreign_keys_controlled(schema, table, control).await;
        let rows = result.as_ref().ok().map(|keys| keys.len() as u64);
        self.audit_controlled_read_result(&operation, start, &result, rows)
            .await?;
        result
    }

    async fn begin(&self) -> Result<Box<dyn Transaction>, DriverError> {
        let inner = self.inner.begin().await?;
        Ok(Box::new(PolicyTransaction {
            guard: self.clone(),
            inner,
            batch_id: Uuid::new_v4(),
        }))
    }

    async fn server_version(&self) -> Result<Option<String>, DriverError> {
        let operation = self.metadata_operation("SERVER VERSION", Vec::new());
        self.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self.inner.server_version().await;
        self.audit_read_result(&operation, start, &result, None).await?;
        result
    }

    async fn ping(&self) -> Result<(), DriverError> {
        self.inner.ping().await
    }

    async fn close(self: Box<Self>) -> Result<(), DriverError> {
        Ok(())
    }
}

impl PolicyGuard {
    async fn execute_write<'a, F, Fut>(
        &'a self,
        sql: &'a str,
        batch_id: Option<Uuid>,
        authorization: Authorization,
        execute: F,
    ) -> Result<ExecResult, DriverError>
    where
        F: FnOnce(&'a Arc<dyn Connection>) -> Fut,
        Fut: std::future::Future<Output = Result<ExecResult, DriverError>>,
    {
        self.require_governed_write_available()?;
        let operation = self.operation(
            sql,
            batch_id,
            &authorization.facts,
            &authorization.decision,
            authorization.approval_outcome,
            authorization.preview_state,
        );
        self.handle_intent_failure(self.record_intent(&operation).await)?;
        let mut pending_write = self.ctx.audit_state.pending_write();
        let start = Instant::now();
        let result = execute(&self.inner).await;
        let rows = result.as_ref().ok().map(|value| value.rows_affected);
        self.audit_write_result(&operation, start, &result, rows).await?;
        pending_write.disarm();
        result
    }

    async fn execute_query_write<'a, F, Fut>(
        &'a self,
        sql: &'a str,
        batch_id: Option<Uuid>,
        authorization: Authorization,
        execute: F,
    ) -> Result<QueryResult, DriverError>
    where
        F: FnOnce(&'a Arc<dyn Connection>) -> Fut,
        Fut: std::future::Future<Output = Result<QueryResult, DriverError>>,
    {
        self.require_governed_write_available()?;
        let operation = self.operation(
            sql,
            batch_id,
            &authorization.facts,
            &authorization.decision,
            authorization.approval_outcome,
            authorization.preview_state,
        );
        self.handle_intent_failure(self.record_intent(&operation).await)?;
        let mut pending_write = self.ctx.audit_state.pending_write();
        let start = Instant::now();
        let result = execute(&self.inner)
            .await
            .map(|value| self.mask_result_for_sql(Some(sql), value));
        let rows = result.as_ref().ok().map(|value| value.rows.len() as u64);
        self.audit_write_result(&operation, start, &result, rows).await?;
        pending_write.disarm();
        result
    }
}

#[async_trait]
impl Transaction for PolicyTransaction {
    async fn query(&mut self, sql: &str) -> Result<QueryResult, DriverError> {
        let authorization = self.guard.authorize(sql, false, None).await?;
        if authorization.facts.writes {
            self.guard.require_governed_write_available()?;
            let operation = self.guard.operation(
                sql,
                Some(self.batch_id),
                &authorization.facts,
                &authorization.decision,
                authorization.approval_outcome,
                authorization.preview_state,
            );
            self.guard
                .handle_intent_failure(self.guard.record_intent(&operation).await)?;
            let mut pending_write = self.guard.ctx.audit_state.pending_write();
            let start = Instant::now();
            let result = self
                .inner
                .query(sql)
                .await
                .map(|value| self.guard.mask_result_for_sql(Some(sql), value));
            let rows = result.as_ref().ok().map(|value| value.rows.len() as u64);
            self.guard
                .audit_transaction_result(&operation, start, &result, rows)
                .await?;
            pending_write.disarm();
            return result;
        }
        let operation = self.guard.operation(
            sql,
            Some(self.batch_id),
            &authorization.facts,
            &authorization.decision,
            authorization.approval_outcome,
            authorization.preview_state,
        );
        self.guard.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self
            .inner
            .query(sql)
            .await
            .map(|value| self.guard.mask_result_for_sql(Some(sql), value));
        let rows = result.as_ref().ok().map(|value| value.rows.len() as u64);
        self.guard
            .audit_transaction_result(&operation, start, &result, rows)
            .await?;
        result
    }

    async fn query_controlled(&mut self, sql: &str, control: &OperationControl) -> Result<QueryResult, DriverError> {
        let authorization = self.guard.authorize(sql, false, Some(control)).await?;
        if authorization.facts.writes {
            self.guard.require_governed_write_available()?;
            let operation = self.guard.operation(
                sql,
                Some(self.batch_id),
                &authorization.facts,
                &authorization.decision,
                authorization.approval_outcome,
                authorization.preview_state,
            );
            self.guard
                .handle_intent_failure(self.guard.record_intent(&operation).await)?;
            let mut pending_write = self.guard.ctx.audit_state.pending_write();
            let start = Instant::now();
            let result = self
                .inner
                .query_controlled(sql, control)
                .await
                .map(|value| self.guard.mask_result_for_sql(Some(sql), value));
            let rows = result.as_ref().ok().map(|value| value.rows.len() as u64);
            self.guard
                .audit_transaction_result(&operation, start, &result, rows)
                .await?;
            pending_write.disarm();
            return result;
        }
        let operation = self.guard.operation(
            sql,
            Some(self.batch_id),
            &authorization.facts,
            &authorization.decision,
            authorization.approval_outcome,
            authorization.preview_state,
        );
        self.guard.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self
            .inner
            .query_controlled(sql, control)
            .await
            .map(|value| self.guard.mask_result_for_sql(Some(sql), value));
        let rows = result.as_ref().ok().map(|value| value.rows.len() as u64);
        self.guard
            .audit_transaction_result(&operation, start, &result, rows)
            .await?;
        result
    }

    async fn query_params(&mut self, sql: &str, params: &[Value]) -> Result<QueryResult, DriverError> {
        let authorization = self.guard.authorize(sql, true, None).await?;
        if authorization.facts.writes {
            self.guard.require_governed_write_available()?;
            let operation = self.guard.operation(
                sql,
                Some(self.batch_id),
                &authorization.facts,
                &authorization.decision,
                authorization.approval_outcome,
                authorization.preview_state,
            );
            self.guard
                .handle_intent_failure(self.guard.record_intent(&operation).await)?;
            let mut pending_write = self.guard.ctx.audit_state.pending_write();
            let start = Instant::now();
            let result = self
                .inner
                .query_params(sql, params)
                .await
                .map(|value| self.guard.mask_result_for_sql(Some(sql), value));
            let rows = result.as_ref().ok().map(|value| value.rows.len() as u64);
            self.guard
                .audit_transaction_result(&operation, start, &result, rows)
                .await?;
            pending_write.disarm();
            return result;
        }
        let operation = self.guard.operation(
            sql,
            Some(self.batch_id),
            &authorization.facts,
            &authorization.decision,
            authorization.approval_outcome,
            authorization.preview_state,
        );
        self.guard.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self
            .inner
            .query_params(sql, params)
            .await
            .map(|value| self.guard.mask_result_for_sql(Some(sql), value));
        let rows = result.as_ref().ok().map(|value| value.rows.len() as u64);
        self.guard
            .audit_transaction_result(&operation, start, &result, rows)
            .await?;
        result
    }

    async fn query_params_controlled(
        &mut self,
        sql: &str,
        params: &[Value],
        control: &OperationControl,
    ) -> Result<QueryResult, DriverError> {
        let authorization = self.guard.authorize(sql, true, Some(control)).await?;
        if authorization.facts.writes {
            self.guard.require_governed_write_available()?;
            let operation = self.guard.operation(
                sql,
                Some(self.batch_id),
                &authorization.facts,
                &authorization.decision,
                authorization.approval_outcome,
                authorization.preview_state,
            );
            self.guard
                .handle_intent_failure(self.guard.record_intent(&operation).await)?;
            let mut pending_write = self.guard.ctx.audit_state.pending_write();
            let start = Instant::now();
            let result = self
                .inner
                .query_params_controlled(sql, params, control)
                .await
                .map(|value| self.guard.mask_result_for_sql(Some(sql), value));
            let rows = result.as_ref().ok().map(|value| value.rows.len() as u64);
            self.guard
                .audit_transaction_result(&operation, start, &result, rows)
                .await?;
            pending_write.disarm();
            return result;
        }
        let operation = self.guard.operation(
            sql,
            Some(self.batch_id),
            &authorization.facts,
            &authorization.decision,
            authorization.approval_outcome,
            authorization.preview_state,
        );
        self.guard.prepare_governed_read(&operation).await?;
        let start = Instant::now();
        let result = self
            .inner
            .query_params_controlled(sql, params, control)
            .await
            .map(|value| self.guard.mask_result_for_sql(Some(sql), value));
        let rows = result.as_ref().ok().map(|value| value.rows.len() as u64);
        self.guard
            .audit_transaction_result(&operation, start, &result, rows)
            .await?;
        result
    }

    async fn execute(&mut self, sql: &str) -> Result<ExecResult, DriverError> {
        let authorization = self.guard.authorize(sql, false, None).await?;
        self.guard.require_governed_write_available()?;
        let operation = self.guard.operation(
            sql,
            Some(self.batch_id),
            &authorization.facts,
            &authorization.decision,
            authorization.approval_outcome,
            authorization.preview_state,
        );
        self.guard
            .handle_intent_failure(self.guard.record_intent(&operation).await)?;
        let mut pending_write = self.guard.ctx.audit_state.pending_write();
        let start = Instant::now();
        let result = self.inner.execute(sql).await;
        let rows = result.as_ref().ok().map(|value| value.rows_affected);
        self.guard
            .audit_transaction_result(&operation, start, &result, rows)
            .await?;
        pending_write.disarm();
        result
    }

    async fn execute_controlled(&mut self, sql: &str, control: &OperationControl) -> Result<ExecResult, DriverError> {
        let authorization = self.guard.authorize(sql, false, Some(control)).await?;
        self.guard.require_governed_write_available()?;
        let operation = self.guard.operation(
            sql,
            Some(self.batch_id),
            &authorization.facts,
            &authorization.decision,
            authorization.approval_outcome,
            authorization.preview_state,
        );
        self.guard
            .handle_intent_failure(self.guard.record_intent(&operation).await)?;
        let mut pending_write = self.guard.ctx.audit_state.pending_write();
        let start = Instant::now();
        let result = self.inner.execute_controlled(sql, control).await;
        let rows = result.as_ref().ok().map(|value| value.rows_affected);
        self.guard
            .audit_transaction_result(&operation, start, &result, rows)
            .await?;
        pending_write.disarm();
        result
    }

    async fn execute_params(&mut self, sql: &str, params: &[Value]) -> Result<ExecResult, DriverError> {
        let authorization = self.guard.authorize(sql, true, None).await?;
        self.guard.require_governed_write_available()?;
        let operation = self.guard.operation(
            sql,
            Some(self.batch_id),
            &authorization.facts,
            &authorization.decision,
            authorization.approval_outcome,
            authorization.preview_state,
        );
        self.guard
            .handle_intent_failure(self.guard.record_intent(&operation).await)?;
        let mut pending_write = self.guard.ctx.audit_state.pending_write();
        let start = Instant::now();
        let result = self.inner.execute_params(sql, params).await;
        let rows = result.as_ref().ok().map(|value| value.rows_affected);
        self.guard
            .audit_transaction_result(&operation, start, &result, rows)
            .await?;
        pending_write.disarm();
        result
    }

    async fn execute_params_controlled(
        &mut self,
        sql: &str,
        params: &[Value],
        control: &OperationControl,
    ) -> Result<ExecResult, DriverError> {
        let authorization = self.guard.authorize(sql, true, Some(control)).await?;
        self.guard.require_governed_write_available()?;
        let operation = self.guard.operation(
            sql,
            Some(self.batch_id),
            &authorization.facts,
            &authorization.decision,
            authorization.approval_outcome,
            authorization.preview_state,
        );
        self.guard
            .handle_intent_failure(self.guard.record_intent(&operation).await)?;
        let mut pending_write = self.guard.ctx.audit_state.pending_write();
        let start = Instant::now();
        let result = self.inner.execute_params_controlled(sql, params, control).await;
        let rows = result.as_ref().ok().map(|value| value.rows_affected);
        self.guard
            .audit_transaction_result(&operation, start, &result, rows)
            .await?;
        pending_write.disarm();
        result
    }

    async fn commit(self: Box<Self>) -> Result<(), DriverError> {
        self.guard.require_governed_write_available()?;
        let operation = PolicyGuard::commit_operation(self.batch_id);
        self.guard.record_intent(&operation).await.map_err(|error| {
            DriverError::PolicyDenied(format!(
                "commit denied because audit intent could not be persisted: {error}"
            ))
        })?;
        let mut pending_write = self.guard.ctx.audit_state.pending_write();
        let start = Instant::now();
        let result = self.inner.commit().await;
        let (status, transaction_outcome, category) = match &result {
            Ok(()) => (AuditTerminalStatus::Succeeded, AuditTransactionOutcome::Committed, None),
            Err(error) if is_ambiguous_post_dispatch(error) => {
                self.guard.ctx.audit_state.disable_governed_writes();
                (
                    AuditTerminalStatus::Unknown,
                    AuditTransactionOutcome::Unknown,
                    Some(error_category(error)),
                )
            }
            Err(error) => (
                AuditTerminalStatus::Failed,
                AuditTransactionOutcome::Failed,
                Some(error_category(error)),
            ),
        };
        let audit_result = self
            .guard
            .record_outcome(
                &operation,
                AuditOutcome {
                    terminal_status: status,
                    transaction_outcome,
                    rows_affected: None,
                    error_category: category,
                    duration_ms: start.elapsed().as_millis() as u64,
                },
            )
            .await;
        self.guard.handle_write_outcome_failure(audit_result)?;
        pending_write.disarm();
        result
    }

    async fn rollback(self: Box<Self>) -> Result<(), DriverError> {
        let operation = AuditOperation {
            operation_id: Uuid::new_v4(),
            batch_id: Some(self.batch_id),
            sql: "ROLLBACK",
            class: AuditOperationClass::TransactionRollback,
            targets: Vec::new(),
            decision_rule: "transaction_rollback".into(),
            approval_outcome: AuditApprovalOutcome::NotRequired,
            preview_state: AuditPreviewState::NotRequested,
        };
        self.guard.record_intent(&operation).await.map_err(|error| {
            DriverError::PolicyDenied(format!(
                "rollback denied because audit intent could not be persisted: {error}"
            ))
        })?;
        let mut pending_write = self.guard.ctx.audit_state.pending_write();
        let start = Instant::now();
        let result = self.inner.rollback().await;
        let (status, transaction_outcome, category) = match &result {
            Ok(()) => (
                AuditTerminalStatus::Succeeded,
                AuditTransactionOutcome::RolledBack,
                None,
            ),
            Err(error) if is_ambiguous_post_dispatch(error) => {
                self.guard.ctx.audit_state.disable_governed_writes();
                (
                    AuditTerminalStatus::Unknown,
                    AuditTransactionOutcome::Unknown,
                    Some(error_category(error)),
                )
            }
            Err(error) => (
                AuditTerminalStatus::Failed,
                AuditTransactionOutcome::Failed,
                Some(error_category(error)),
            ),
        };
        let audit_result = self
            .guard
            .record_outcome(
                &operation,
                AuditOutcome {
                    terminal_status: status,
                    transaction_outcome,
                    rows_affected: None,
                    error_category: category,
                    duration_ms: start.elapsed().as_millis() as u64,
                },
            )
            .await;
        self.guard.handle_write_outcome_failure(audit_result)?;
        pending_write.disarm();
        result
    }

    async fn rollback_controlled(self: Box<Self>, control: &OperationControl) -> Result<(), DriverError> {
        let operation = AuditOperation {
            operation_id: Uuid::new_v4(),
            batch_id: Some(self.batch_id),
            sql: "ROLLBACK",
            class: AuditOperationClass::TransactionRollback,
            targets: Vec::new(),
            decision_rule: "transaction_rollback".into(),
            approval_outcome: AuditApprovalOutcome::NotRequired,
            preview_state: AuditPreviewState::NotRequested,
        };
        self.guard.record_intent(&operation).await.map_err(|error| {
            DriverError::PolicyDenied(format!(
                "rollback denied because audit intent could not be persisted: {error}"
            ))
        })?;
        let mut pending_write = self.guard.ctx.audit_state.pending_write();
        let start = Instant::now();
        let result = self.inner.rollback_controlled(control).await;
        let (status, transaction_outcome, category) = match &result {
            Ok(()) => (
                AuditTerminalStatus::Succeeded,
                AuditTransactionOutcome::RolledBack,
                None,
            ),
            Err(error) => {
                self.guard.ctx.audit_state.disable_governed_writes();
                (
                    AuditTerminalStatus::Unknown,
                    AuditTransactionOutcome::Unknown,
                    Some(error_category(error)),
                )
            }
        };
        let audit_result = self
            .guard
            .record_outcome(
                &operation,
                AuditOutcome {
                    terminal_status: status,
                    transaction_outcome,
                    rows_affected: None,
                    error_category: category,
                    duration_ms: start.elapsed().as_millis() as u64,
                },
            )
            .await;
        self.guard.handle_write_outcome_failure(audit_result)?;
        pending_write.disarm();
        result
    }
}
