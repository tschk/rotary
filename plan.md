1. **Understand the problem:** The task is to ensure `todos` are correctly saved and loaded in SQLite for sessions. The missing logic is in `Session::save_sqlite` and `Session::load_sqlite`.
   I need to alter the `sessions` table in `save_sqlite` and `load_sqlite` similarly to `projections` and `artifacts`, but for `todos`.
2. **Implementation details:**
   In `save_sqlite`:
   - Add `ALTER TABLE sessions ADD COLUMN todos TEXT`.
   - Update `todos` via `UPDATE sessions SET todos = ?1 WHERE id = ?2`.

   In `load_sqlite`:
   - Execute `ALTER TABLE sessions ADD COLUMN todos TEXT`.
   - Query `todos` and update `session.todos`.
3. **Tests:**
   Add `test_sqlite_todo_state` inside `mod tests` with `#[cfg(feature = "sqlite-sessions")]`.
