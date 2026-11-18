package sqlite

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"net/url"
	"path/filepath"
	"strings"
	"sync"
	"time"

	_ "modernc.org/sqlite"
	"recallport/internal/memory"
)

const schema = `
CREATE TABLE memories (
 id TEXT UNIQUE NOT NULL, body TEXT NOT NULL, circumstances TEXT NOT NULL,
 occurred TEXT NOT NULL, start_day TEXT NOT NULL, end_day TEXT NOT NULL,
 origin TEXT NOT NULL, source TEXT NOT NULL, recorded TEXT NOT NULL, revision INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX timeline ON memories(start_day DESC,recorded DESC,id DESC);
CREATE INDEX memory_origin ON memories(origin,occurred) WHERE origin <> '';
CREATE TABLE refs (
 source_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
 target_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
 reason TEXT NOT NULL, PRIMARY KEY(source_id,target_id)
);
CREATE INDEX reverse_refs ON refs(target_id);
CREATE TABLE requests (
 actor TEXT NOT NULL, request_id TEXT NOT NULL, fingerprint BLOB NOT NULL,
 memory_id TEXT NOT NULL REFERENCES memories(id) ON DELETE CASCADE,
 PRIMARY KEY(actor,request_id)
);
` + vectorSchema + ftsSchema + `PRAGMA user_version=2;`

const vectorSchema = `CREATE TABLE vectors (
 memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
 model TEXT NOT NULL, vector BLOB NOT NULL
);`

const ftsSchema = `CREATE VIRTUAL TABLE memory_fts USING fts5(body,circumstances,content='memories',tokenize='trigram');
CREATE TRIGGER index_insert AFTER INSERT ON memories BEGIN
 INSERT INTO memory_fts(rowid,body,circumstances) VALUES(new.rowid,new.body,new.circumstances);
END;
CREATE TRIGGER index_delete AFTER DELETE ON memories BEGIN
 INSERT INTO memory_fts(memory_fts,rowid,body,circumstances) VALUES('delete',old.rowid,old.body,old.circumstances);
END;
CREATE TRIGGER index_update AFTER UPDATE OF body,circumstances ON memories BEGIN
 INSERT INTO memory_fts(memory_fts,rowid,body,circumstances) VALUES('delete',old.rowid,old.body,old.circumstances);
 INSERT INTO memory_fts(rowid,body,circumstances) VALUES(new.rowid,new.body,new.circumstances);
END;
`

const schemaVersion = 2

type Store struct {
	db          *sql.DB
	embed       memory.Embedder
	path        string
	maintenance sync.Mutex
	index       indexManager
	vectorBytes int
}

func Open(path string, embed memory.Embedder) (*Store, error) {
	dsn, err := sqliteURI(path, "_pragma=foreign_keys(1)&_pragma=busy_timeout(5000)&_pragma=secure_delete(ON)")
	if err != nil {
		return nil, err
	}
	db, err := sql.Open("sqlite", dsn)
	if err != nil {
		return nil, err
	}
	db.SetMaxOpenConns(1)
	s := &Store{db: db, embed: embed, path: path, index: indexManager{wake: make(chan struct{}, 1)}}
	if embed != nil {
		s.vectorBytes = 4 * len(embed.Embed("RecallPort index"))
	}
	if err := s.init(context.Background()); err != nil {
		db.Close()
		return nil, err
	}
	return s, nil
}

func sqliteURI(path, query string) (string, error) {
	path, err := filepath.Abs(path)
	if err != nil {
		return "", err
	}
	path = filepath.ToSlash(path)
	if !strings.HasPrefix(path, "/") {
		path = "/" + path
	}
	dsn := url.URL{Scheme: "file", Path: path, RawQuery: query}
	return dsn.String(), nil
}

func (s *Store) init(ctx context.Context) error {
	if _, err := s.db.ExecContext(ctx, "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL"); err != nil {
		return err
	}
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	var ver int
	if err := tx.QueryRowContext(ctx, "PRAGMA user_version").Scan(&ver); err != nil {
		return err
	}
	switch ver {
	case 0:
		if _, err := tx.ExecContext(ctx, schema); err != nil {
			return err
		}
	case 1:
		if _, err := tx.ExecContext(ctx, "DROP TABLE IF EXISTS credentials; PRAGMA user_version=2"); err != nil {
			return err
		}
	case schemaVersion:
	default:
		return fmt.Errorf("unsupported memory database version %d", ver)
	}
	return tx.Commit()
}

func (s *Store) Close() error { return s.db.Close() }

func (s *Store) Snapshot(ctx context.Context, destination string) error {
	_, err := s.db.ExecContext(ctx, "VACUUM INTO ?", destination)
	return err
}

func newID() string { var b [16]byte; rand.Read(b[:]); return hex.EncodeToString(b[:]) }

func (s *Store) UsesVectors() bool { return s.embed != nil }

func (s *Store) Remember(ctx context.Context, actor memory.Actor, d memory.Draft) (memory.Entry, error) {
	d, err := memory.CleanDraft(d)
	if err != nil {
		return memory.Entry{}, err
	}
	lo, hi, _ := memory.DateRange(d.Occurred)
	body, _ := json.Marshal(d)
	h := sha256.Sum256(body)
	v, err := s.makeVector(d.Text, d.Context)
	if err != nil {
		return memory.Entry{}, err
	}
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return memory.Entry{}, err
	}
	defer tx.Rollback()
	if d.RequestID != "" {
		var old []byte
		var id string
		err := tx.QueryRowContext(ctx, "SELECT fingerprint,memory_id FROM requests WHERE actor=? AND request_id=?", actor.ID, d.RequestID).Scan(&old, &id)
		if err == nil {
			if string(old) != string(h[:]) {
				return memory.Entry{}, memory.ErrConflict
			}
			return readEntry(ctx, tx, id)
		}
		if !errors.Is(err, sql.ErrNoRows) {
			return memory.Entry{}, err
		}
	}
	id := newID()
	if _, err = tx.ExecContext(ctx, `INSERT INTO memories(id,body,circumstances,occurred,start_day,end_day,origin,source,recorded) VALUES(?,?,?,?,?,?,?,?,?)`, id, d.Text, d.Context, d.Occurred, lo, hi, d.Origin, actor.Label, time.Now().UTC().Format("2006-01-02T15:04:05.000000000Z")); err != nil {
		return memory.Entry{}, err
	}
	for _, r := range d.References {
		res, err := tx.ExecContext(ctx, "INSERT INTO refs(source_id,target_id,reason) SELECT ?,id,? FROM memories WHERE id=? AND id<>?", id, r.Reason, r.ID, id)
		if err != nil {
			return memory.Entry{}, err
		}
		n, _ := res.RowsAffected()
		if n == 0 {
			return memory.Entry{}, fmt.Errorf("%w: referenced memory does not exist", memory.ErrInvalid)
		}
	}
	if d.RequestID != "" {
		if _, err = tx.ExecContext(ctx, "INSERT INTO requests(actor,request_id,fingerprint,memory_id) VALUES(?,?,?,?)", actor.ID, d.RequestID, h[:], id); err != nil {
			return memory.Entry{}, err
		}
	}
	if err = s.setVector(ctx, tx, id, v); err != nil {
		return memory.Entry{}, err
	}
	got, err := readEntry(ctx, tx, id)
	if err != nil {
		return memory.Entry{}, err
	}
	if err := tx.Commit(); err != nil {
		return memory.Entry{}, err
	}
	return got, nil
}

func (s *Store) Edit(ctx context.Context, id string, rev int, d memory.Draft) error {
	d, err := memory.CleanDraft(d)
	if err != nil {
		return err
	}
	lo, hi, _ := memory.DateRange(d.Occurred)
	v, err := s.makeVector(d.Text, d.Context)
	if err != nil {
		return err
	}
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	res, err := tx.ExecContext(ctx, `UPDATE memories SET body=?,circumstances=?,occurred=?,start_day=?,end_day=?,revision=revision+1 WHERE id=? AND revision=?`, d.Text, d.Context, d.Occurred, lo, hi, id, rev)
	if err != nil {
		return err
	}
	n, _ := res.RowsAffected()
	if n == 0 {
		return memory.ErrConflict
	}
	if err = s.setVector(ctx, tx, id, v); err != nil {
		return err
	}
	return tx.Commit()
}

func (s *Store) Delete(ctx context.Context, id string, rev int) error {
	res, err := s.db.ExecContext(ctx, "DELETE FROM memories WHERE id=? AND revision=?", id, rev)
	if err != nil {
		return err
	}
	n, _ := res.RowsAffected()
	if n == 0 {
		return memory.ErrConflict
	}
	return nil
}

const fields = "m.id,m.body,m.circumstances,m.occurred,m.origin,m.source,m.recorded,m.revision"

type scanner interface{ Scan(...any) error }

func scanEntry(r scanner) (memory.Entry, error) {
	var e memory.Entry
	err := r.Scan(&e.ID, &e.Text, &e.Context, &e.Occurred, &e.Origin, &e.Source, &e.Recorded, &e.Revision)
	if errors.Is(err, sql.ErrNoRows) {
		err = memory.ErrNotFound
	}
	return e, err
}

type querier interface {
	QueryContext(context.Context, string, ...any) (*sql.Rows, error)
	QueryRowContext(context.Context, string, ...any) *sql.Row
}

func readEntry(ctx context.Context, q querier, id string) (memory.Entry, error) {
	e, err := scanEntry(q.QueryRowContext(ctx, "SELECT "+fields+" FROM memories m WHERE m.id=?", id))
	if err != nil {
		return e, err
	}
	for i, query := range []string{"SELECT target_id,reason FROM refs WHERE source_id=? ORDER BY target_id", "SELECT source_id,reason FROM refs WHERE target_id=? ORDER BY source_id LIMIT 30"} {
		rows, err := q.QueryContext(ctx, query, id)
		if err != nil {
			return e, err
		}
		for rows.Next() {
			var r memory.Reference
			if err = rows.Scan(&r.ID, &r.Reason); err != nil {
				rows.Close()
				return e, err
			}
			if i == 0 {
				e.References = append(e.References, r)
			} else {
				e.CitedBy = append(e.CitedBy, r)
			}
		}
		if err = rows.Err(); err != nil {
			rows.Close()
			return e, err
		}
		rows.Close()
	}
	return e, nil
}

func (s *Store) Read(ctx context.Context, id string) (memory.Entry, error) {
	tx, err := s.db.BeginTx(ctx, &sql.TxOptions{ReadOnly: true})
	if err != nil {
		return memory.Entry{}, err
	}
	defer tx.Rollback()
	return readEntry(ctx, tx, id)
}
