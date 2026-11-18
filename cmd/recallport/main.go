package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	"os"
	"os/signal"
	"path/filepath"
	"strings"
	"time"

	"recallport/internal/embedding"
	"recallport/internal/memory"
	"recallport/internal/sqlite"
	"recallport/internal/transport"
)

func main() {
	if err := run(os.Args[1:], os.Stdout); err != nil {
		log.Print(err)
		os.Exit(1)
	}
}

func run(args []string, out io.Writer) error {
	command := "serve"
	if len(args) > 0 && !strings.HasPrefix(args[0], "-") {
		command, args = args[0], args[1:]
	}
	if command != "serve" && command != "restore" {
		return fmt.Errorf("usage: recallport serve [-data-dir PATH] [-port 3765] or recallport restore -from BACKUP -data-dir NEW_PATH")
	}
	exe, err := os.Executable()
	if err != nil {
		return err
	}
	f := flag.NewFlagSet(command, flag.ContinueOnError)
	if command == "restore" {
		from := f.String("from", "", "RecallPort backup file")
		data := f.String("data-dir", "", "new data directory")
		if err := f.Parse(args); err != nil {
			return err
		}
		if *from == "" || *data == "" || f.NArg() != 0 {
			return fmt.Errorf("restore requires -from BACKUP and -data-dir NEW_PATH")
		}
		if err := sqlite.Restore(context.Background(), *from, *data); err != nil {
			return err
		}
		fmt.Fprintf(out, "Restored: %s\n", filepath.Join(*data, "memory.db"))
		return nil
	}
	data := f.String("data-dir", filepath.Join(filepath.Dir(exe), "data"), "private data directory")
	port := f.Int("port", 3765, "loopback HTTP port")
	if err = f.Parse(args); err != nil {
		return err
	}
	if f.NArg() != 0 || *port < 1 || *port > 65535 {
		return fmt.Errorf("invalid arguments")
	}
	if err = os.MkdirAll(*data, 0700); err != nil {
		return err
	}
	listener, err := net.Listen("tcp4", fmt.Sprintf("127.0.0.1:%d", *port))
	if err != nil {
		return err
	}
	defer listener.Close()
	var embed memory.Embedder
	model, err := embedding.Open()
	if err != nil {
		return err
	}
	if model != nil {
		embed = model
	}
	store, err := sqlite.Open(filepath.Join(*data, "memory.db"), embed)
	if err != nil {
		return err
	}
	defer store.Close()
	h, err := transport.New(memory.New(store), listener.Addr().String())
	if err != nil {
		return err
	}
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt)
	defer stop()
	ctx, cancel := context.WithCancel(ctx)
	done := make(chan struct{})
	go func() {
		defer close(done)
		store.MaintainIndex(ctx)
	}()
	defer func() { cancel(); <-done }()
	srv := &http.Server{Handler: h, ReadHeaderTimeout: 5 * time.Second, ReadTimeout: 30 * time.Second, WriteTimeout: 60 * time.Second, IdleTimeout: 45 * time.Second, MaxHeaderBytes: 32 << 10}
	go func() {
		<-ctx.Done()
		shutdown, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_ = srv.Shutdown(shutdown)
	}()
	fmt.Fprintf(out, "RecallPort  http://%s/\nMCP         http://%s/mcp\n", listener.Addr(), listener.Addr())
	if err = srv.Serve(listener); !errors.Is(err, http.ErrServerClosed) {
		return err
	}
	return nil
}
