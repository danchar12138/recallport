package memory

import (
	"context"
	"io"
	"time"
)

const MaxBackupBytes = 1 << 30

type Reader interface {
	Read(context.Context, string) (Entry, error)
	Recall(context.Context, Query) (Results, error)
}

type Writer interface {
	Remember(context.Context, Actor, Draft) (Entry, error)
	Edit(context.Context, string, int, Draft) error
	Delete(context.Context, string, int) error
	SetReference(context.Context, string, Reference, bool) error
}

type Period struct {
	Date  string
	Count int
}

type Browse interface {
	Timeline(context.Context, Query, int) (Results, error)
	Periods(context.Context) ([]Period, error)
	Related(context.Context, string) ([]Hit, error)
	UsesVectors() bool
}

type Data interface {
	Snapshot(context.Context, string) error
	Replace(context.Context, io.Reader) error
	OpenRecovery() (io.ReadSeekCloser, string, error)
	RequestRebuild() bool
	IndexStatus(context.Context) IndexStatus
}

type IndexStatus struct {
	State      string     `json:"state"`
	Keyword    string     `json:"keyword"`
	Model      string     `json:"model"`
	Indexed    int        `json:"indexed"`
	Total      int        `json:"total"`
	Skipped    int        `json:"skipped"`
	CheckedAt  *time.Time `json:"checked_at,omitempty"`
	RepairedAt *time.Time `json:"repaired_at,omitempty"`
	Problem    string     `json:"problem,omitempty"`
}

type Repository interface {
	Reader
	Writer
	Browse
	Data
}

type Service struct{ Repository }

func New(repo Repository) *Service { return &Service{Repository: repo} }

func (s *Service) Remember(ctx context.Context, actor Actor, draft Draft) (Entry, error) {
	d, err := CleanDraft(draft)
	if err != nil {
		return Entry{}, err
	}
	return s.Repository.Remember(ctx, actor, d)
}

func (s *Service) Edit(ctx context.Context, id string, rev int, draft Draft) error {
	d, err := CleanDraft(draft)
	if err != nil {
		return err
	}
	return s.Repository.Edit(ctx, id, rev, d)
}
