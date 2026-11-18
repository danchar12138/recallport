package sqlite

import (
	"context"
	"database/sql"
	"fmt"
	"io"
	"os"
	"path/filepath"
)

// Restore installs a standalone snapshot into a new directory. It never replaces a live database.
func Restore(ctx context.Context, source, destination string) error {
	in, err := os.Open(source)
	if err != nil {
		return err
	}
	defer in.Close()
	info, err := in.Stat()
	if err != nil {
		return err
	}
	if !info.Mode().IsRegular() {
		return fmt.Errorf("backup must be a regular file")
	}
	if err := os.Mkdir(destination, 0700); err != nil {
		return fmt.Errorf("restore requires a new data directory: %w", err)
	}
	complete := false
	defer func() {
		if !complete {
			_ = os.Remove(destination)
		}
	}()
	f, err := os.CreateTemp(destination, ".restore-*")
	if err != nil {
		return err
	}
	defer os.Remove(f.Name())
	_, copyErr := io.Copy(f, in)
	if copyErr == nil {
		copyErr = f.Sync()
	}
	closeErr := f.Close()
	if copyErr != nil {
		return copyErr
	}
	if closeErr != nil {
		return closeErr
	}
	if err := validateBackup(ctx, f.Name()); err != nil {
		return fmt.Errorf("invalid RecallPort backup: %w", err)
	}
	if err := os.Rename(f.Name(), filepath.Join(destination, "memory.db")); err != nil {
		return err
	}
	complete = true
	return nil
}

func validateBackup(ctx context.Context, path string) error {
	dsn, err := sqliteURI(path, "mode=ro&immutable=1&_pragma=trusted_schema(OFF)")
	if err != nil {
		return err
	}
	db, err := sql.Open("sqlite", dsn)
	if err != nil {
		return err
	}
	defer db.Close()
	var version int
	if err := db.QueryRowContext(ctx, "PRAGMA user_version").Scan(&version); err != nil {
		return err
	}
	if version < 1 || version > schemaVersion {
		return fmt.Errorf("unsupported database version %d", version)
	}
	var objects int
	if err := db.QueryRowContext(ctx, `SELECT count(*) FROM sqlite_schema WHERE
		(type='table' AND name IN ('memories','refs','requests','vectors','memory_fts'))
		OR (type='trigger' AND name IN ('index_insert','index_update','index_delete'))`).Scan(&objects); err != nil {
		return err
	}
	if objects != 8 {
		return fmt.Errorf("memory tables or indexes are missing")
	}
	var check string
	if err := db.QueryRowContext(ctx, "PRAGMA quick_check(1)").Scan(&check); err != nil {
		return err
	}
	if check != "ok" {
		return fmt.Errorf("SQLite integrity check failed")
	}
	return nil
}
