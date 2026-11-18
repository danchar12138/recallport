package sqlite

import (
	"context"
	"database/sql"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"time"

	"recallport/internal/memory"
)

func (s *Store) Replace(ctx context.Context, input io.Reader) error {
	f, err := os.CreateTemp(filepath.Dir(s.path), ".restore-*")
	if err != nil {
		return err
	}
	defer os.Remove(f.Name())
	defer f.Close()
	n, err := io.Copy(f, io.LimitReader(input, memory.MaxBackupBytes+1))
	if err != nil {
		return fmt.Errorf("%w: upload could not be read", memory.ErrBackup)
	}
	if err = f.Close(); err != nil {
		return err
	}
	if n > memory.MaxBackupBytes {
		return fmt.Errorf("%w: snapshot exceeds 1 GiB", memory.ErrBackup)
	}
	if err = validateBackup(ctx, f.Name()); err != nil {
		return fmt.Errorf("%w: %v", memory.ErrBackup, err)
	}
	dsn, err := sqliteURI(f.Name(), "mode=ro&immutable=1&_pragma=trusted_schema(OFF)")
	if err != nil {
		return err
	}
	source, err := sql.Open("sqlite", dsn)
	if err != nil {
		return err
	}
	defer source.Close()
	// Hold the only live connection across the safety snapshot and replacement transaction.
	s.maintenance.Lock()
	defer s.maintenance.Unlock()
	conn, err := s.db.Conn(ctx)
	if err != nil {
		return err
	}
	defer conn.Close()
	dir := filepath.Join(filepath.Dir(s.path), "recovery")
	if err = os.MkdirAll(dir, 0700); err != nil {
		return err
	}
	name := "before-restore-" + time.Now().UTC().Format("20060102-150405.000000000") + "-" + newID()[:8] + ".db"
	tmp, err := os.CreateTemp(dir, ".snapshot-*")
	if err != nil {
		return err
	}
	defer os.Remove(tmp.Name())
	if err = tmp.Close(); err != nil {
		return err
	}
	if _, err = conn.ExecContext(ctx, `VACUUM INTO ?`, tmp.Name()); err != nil {
		return err
	}
	if err = os.Rename(tmp.Name(), filepath.Join(dir, name)); err != nil {
		return err
	}
	tx, err := conn.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	if _, err = tx.ExecContext(ctx, `DELETE FROM memories`); err != nil {
		return err
	}
	rows, err := source.QueryContext(ctx, `SELECT id,body,circumstances,occurred,origin,source,recorded,revision FROM memories`)
	if err != nil {
		return fmt.Errorf("%w: cannot read memories", memory.ErrBackup)
	}
	for rows.Next() {
		var m memory.Entry
		if err = rows.Scan(&m.ID, &m.Text, &m.Context, &m.Occurred, &m.Origin, &m.Source, &m.Recorded, &m.Revision); err != nil {
			rows.Close()
			return fmt.Errorf("%w: malformed memory", memory.ErrBackup)
		}
		lo, hi, dateErr := memory.DateRange(m.Occurred)
		_, timeErr := time.Parse(time.RFC3339Nano, m.Recorded)
		if dateErr != nil || timeErr != nil || len(m.ID) != 32 || m.Revision < 1 || len(m.Source) > 160 || len(m.Text) > 48_000 || len(m.Context) > 6000 || len(m.Origin) > 256 {
			rows.Close()
			return fmt.Errorf("%w: malformed memory", memory.ErrBackup)
		}
		if _, err = tx.ExecContext(ctx, `INSERT INTO memories(id,body,circumstances,occurred,start_day,end_day,origin,source,recorded,revision) VALUES(?,?,?,?,?,?,?,?,?,?)`, m.ID, m.Text, m.Context, m.Occurred, lo, hi, m.Origin, m.Source, m.Recorded, m.Revision); err != nil {
			rows.Close()
			return err
		}
		if err = s.setVector(ctx, tx, m.ID, s.makeVector(m.Text, m.Context)); err != nil {
			rows.Close()
			return err
		}
	}
	err = rows.Err()
	rows.Close()
	if err != nil {
		return err
	}
	if err = copyTable(ctx, source, tx, `SELECT source_id,target_id,reason FROM refs`, `INSERT INTO refs VALUES(?,?,?)`, 3); err != nil {
		return err
	}
	if err = copyTable(ctx, source, tx, `SELECT actor,request_id,fingerprint,memory_id FROM requests`, `INSERT INTO requests VALUES(?,?,?,?)`, 4); err != nil {
		return err
	}
	return tx.Commit()
}

func copyTable(ctx context.Context, source *sql.DB, tx *sql.Tx, query, insert string, width int) error {
	rows, err := source.QueryContext(ctx, query)
	if err != nil {
		return fmt.Errorf("%w: backup table cannot be read", memory.ErrBackup)
	}
	defer rows.Close()
	values, dest := make([]any, width), make([]any, width)
	for i := range values {
		dest[i] = &values[i]
	}
	for rows.Next() {
		if err = rows.Scan(dest...); err != nil {
			return fmt.Errorf("%w: unreadable backup record", memory.ErrBackup)
		}
		if _, err = tx.ExecContext(ctx, insert, values...); err != nil {
			return fmt.Errorf("%w: inconsistent backup records: %v", memory.ErrBackup, err)
		}
	}
	return rows.Err()
}

func (s *Store) OpenRecovery() (io.ReadSeekCloser, string, error) {
	dir := filepath.Join(filepath.Dir(s.path), "recovery")
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, "", err
	}
	for i := len(entries) - 1; i >= 0; i-- {
		e := entries[i]
		name := e.Name()
		if e.Type().IsRegular() && strings.HasPrefix(name, "before-restore-") && strings.HasSuffix(name, ".db") {
			f, err := os.Open(filepath.Join(dir, name))
			return f, name, err
		}
	}
	return nil, "", os.ErrNotExist
}
