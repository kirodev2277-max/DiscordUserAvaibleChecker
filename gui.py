#!/usr/bin/env python3
"""Tkinter GUI for the Discord username availability checker.

Run with:

    python gui.py

Lets you paste a list of usernames (one per line, or comma-separated, or
both), kicks off a concurrent batch check in a background thread, streams
results into a colored Treeview, and optionally appends them to
``results.txt`` in the same format as the CLI.
"""

from __future__ import annotations

import queue
import threading
from tkinter import (
    BooleanVar,
    StringVar,
    Tk,
    filedialog,
    messagebox,
    ttk,
    scrolledtext,
)
from typing import List

import requests

from checker import (
    DEFAULT_OUTPUT_FILE,
    DEFAULT_WORKERS,
    CheckResult,
    check_many,
    parse_usernames_text,
    save_results,
)


# Polling interval (ms) for draining the worker -> UI message queue.
QUEUE_POLL_MS = 75

# Tag colors for the results tree.
TAG_COLORS = {
    "available": "#1e8449",       # green
    "taken": "#c0392b",            # red
    "invalid": "#d35400",          # orange
    "rate_limited": "#7d6608",     # dark yellow
    "error": "#7f8c8d",            # gray
}


class CheckerGUI:
    def __init__(self, root: Tk) -> None:
        self.root = root
        root.title("Discord Username Checker")
        root.geometry("780x600")
        root.minsize(640, 480)

        # Worker -> UI message queue. Messages are tuples:
        #   ("result", index, CheckResult)
        #   ("done", List[CheckResult])
        #   ("error", str)
        self._queue: "queue.Queue[tuple]" = queue.Queue()
        self._worker: threading.Thread | None = None
        self._total = 0
        self._completed = 0

        self.autosave_var = BooleanVar(value=True)
        self.output_path_var = StringVar(value=DEFAULT_OUTPUT_FILE)
        self.workers_var = StringVar(value=str(DEFAULT_WORKERS))
        self.status_var = StringVar(value="Ready.")

        self._build_widgets()
        self.root.after(QUEUE_POLL_MS, self._drain_queue)

    # ---------- layout ----------

    def _build_widgets(self) -> None:
        root = self.root
        root.columnconfigure(0, weight=1)
        root.rowconfigure(1, weight=1)
        root.rowconfigure(3, weight=2)

        # Input area
        input_frame = ttk.LabelFrame(
            root,
            text="Usernames (one per line, or comma-separated)",
        )
        input_frame.grid(row=0, column=0, sticky="nsew", padx=8, pady=(8, 4))
        input_frame.columnconfigure(0, weight=1)
        input_frame.rowconfigure(0, weight=1)

        self.input_text = scrolledtext.ScrolledText(
            input_frame, height=8, wrap="word"
        )
        self.input_text.grid(row=0, column=0, sticky="nsew", padx=4, pady=4)

        # Options row
        options_frame = ttk.Frame(root)
        options_frame.grid(row=1, column=0, sticky="ew", padx=8, pady=4)
        options_frame.columnconfigure(6, weight=1)

        ttk.Label(options_frame, text="Workers:").grid(row=0, column=0, padx=(0, 4))
        ttk.Spinbox(
            options_frame,
            from_=1,
            to=64,
            width=4,
            textvariable=self.workers_var,
        ).grid(row=0, column=1, padx=(0, 12))

        ttk.Checkbutton(
            options_frame,
            text="Auto-save to:",
            variable=self.autosave_var,
        ).grid(row=0, column=2, padx=(0, 4))
        ttk.Entry(options_frame, textvariable=self.output_path_var, width=24).grid(
            row=0, column=3, padx=(0, 4)
        )
        ttk.Button(
            options_frame, text="Browse...", command=self._on_browse_output
        ).grid(row=0, column=4, padx=(0, 12))

        ttk.Button(
            options_frame, text="Load from file...", command=self._on_load_file
        ).grid(row=0, column=5, padx=(0, 4))

        # Action buttons
        button_frame = ttk.Frame(root)
        button_frame.grid(row=2, column=0, sticky="ew", padx=8, pady=4)
        button_frame.columnconfigure(3, weight=1)

        self.check_button = ttk.Button(
            button_frame, text="Check", command=self._on_check
        )
        self.check_button.grid(row=0, column=0, padx=(0, 4))

        self.save_button = ttk.Button(
            button_frame, text="Save results", command=self._on_save
        )
        self.save_button.grid(row=0, column=1, padx=(0, 4))

        self.clear_button = ttk.Button(
            button_frame, text="Clear", command=self._on_clear
        )
        self.clear_button.grid(row=0, column=2, padx=(0, 4))

        self.progress = ttk.Progressbar(
            button_frame, mode="determinate", length=200
        )
        self.progress.grid(row=0, column=3, sticky="ew", padx=(8, 0))

        # Results table
        results_frame = ttk.LabelFrame(root, text="Results")
        results_frame.grid(row=3, column=0, sticky="nsew", padx=8, pady=(4, 4))
        results_frame.columnconfigure(0, weight=1)
        results_frame.rowconfigure(0, weight=1)

        columns = ("username", "status", "detail")
        self.tree = ttk.Treeview(
            results_frame, columns=columns, show="headings", height=10
        )
        self.tree.heading("username", text="Username")
        self.tree.heading("status", text="Status")
        self.tree.heading("detail", text="Detail")
        self.tree.column("username", width=200, anchor="w")
        self.tree.column("status", width=110, anchor="w")
        self.tree.column("detail", width=400, anchor="w")
        self.tree.grid(row=0, column=0, sticky="nsew", padx=(4, 0), pady=4)

        for tag, color in TAG_COLORS.items():
            self.tree.tag_configure(tag, foreground=color)

        scroll = ttk.Scrollbar(
            results_frame, orient="vertical", command=self.tree.yview
        )
        self.tree.configure(yscrollcommand=scroll.set)
        scroll.grid(row=0, column=1, sticky="ns", padx=(0, 4), pady=4)

        # Status bar
        status_bar = ttk.Label(
            root, textvariable=self.status_var, anchor="w", relief="sunken"
        )
        status_bar.grid(row=4, column=0, sticky="ew", padx=8, pady=(0, 8))

        # Internal: ordered list of completed results, kept in input order so
        # that "Save results" writes the same order as the CLI.
        self._results: List[CheckResult | None] = []

    # ---------- button handlers ----------

    def _on_browse_output(self) -> None:
        path = filedialog.asksaveasfilename(
            defaultextension=".txt",
            filetypes=[("Text files", "*.txt"), ("All files", "*.*")],
            initialfile=self.output_path_var.get() or DEFAULT_OUTPUT_FILE,
        )
        if path:
            self.output_path_var.set(path)

    def _on_load_file(self) -> None:
        path = filedialog.askopenfilename(
            filetypes=[("Text files", "*.txt"), ("All files", "*.*")],
        )
        if not path:
            return
        try:
            with open(path, "r", encoding="utf-8") as fh:
                content = fh.read()
        except OSError as exc:
            messagebox.showerror("Could not read file", str(exc))
            return
        # Append to whatever the user already has.
        existing = self.input_text.get("1.0", "end").rstrip()
        if existing:
            self.input_text.insert("end", "\n")
        self.input_text.insert("end", content)

    def _on_check(self) -> None:
        if self._worker is not None and self._worker.is_alive():
            messagebox.showinfo("Busy", "A check is already running.")
            return

        raw = self.input_text.get("1.0", "end")
        usernames = parse_usernames_text(raw)
        if not usernames:
            messagebox.showwarning(
                "No usernames", "Enter at least one username to check."
            )
            return

        try:
            workers = int(self.workers_var.get())
        except ValueError:
            messagebox.showerror("Invalid workers", "Workers must be an integer.")
            return
        if workers < 1:
            messagebox.showerror("Invalid workers", "Workers must be >= 1.")
            return

        # Reset UI state for a new run.
        for item in self.tree.get_children():
            self.tree.delete(item)
        self._results = [None] * len(usernames)
        self._total = len(usernames)
        self._completed = 0
        self.progress.configure(maximum=self._total, value=0)
        self.status_var.set(f"Checking 0/{self._total}...")
        self.check_button.state(["disabled"])

        # Pre-insert placeholder rows so results land in input order.
        for i, name in enumerate(usernames):
            self.tree.insert(
                "",
                "end",
                iid=str(i),
                values=(name, "pending", ""),
                tags=("pending",),
            )

        self._worker = threading.Thread(
            target=self._run_checks,
            args=(usernames, workers),
            daemon=True,
        )
        self._worker.start()

    def _on_save(self) -> None:
        results = [r for r in self._results if r is not None]
        if not results:
            messagebox.showinfo("Nothing to save", "Run a check first.")
            return
        path = self.output_path_var.get().strip() or DEFAULT_OUTPUT_FILE
        try:
            written = save_results(results, path)
        except OSError as exc:
            messagebox.showerror("Could not save", str(exc))
            return
        self.status_var.set(f"Saved {written} result(s) to {path}")

    def _on_clear(self) -> None:
        self.input_text.delete("1.0", "end")
        for item in self.tree.get_children():
            self.tree.delete(item)
        self._results = []
        self._total = 0
        self._completed = 0
        self.progress.configure(value=0, maximum=100)
        self.status_var.set("Ready.")

    # ---------- worker thread ----------

    def _run_checks(self, usernames: List[str], workers: int) -> None:
        session = requests.Session()
        try:
            def _on_result(index: int, result: CheckResult) -> None:
                self._queue.put(("result", index, result))

            results = check_many(
                usernames,
                workers=workers,
                session=session,
                on_result=_on_result,
            )
            self._queue.put(("done", results))
        except Exception as exc:  # pragma: no cover - defensive
            self._queue.put(("error", str(exc)))
        finally:
            session.close()

    # ---------- queue draining (UI thread) ----------

    def _drain_queue(self) -> None:
        try:
            while True:
                msg = self._queue.get_nowait()
                kind = msg[0]
                if kind == "result":
                    _, index, result = msg
                    self._apply_result(index, result)
                elif kind == "done":
                    _, results = msg
                    self._on_run_finished(results)
                elif kind == "error":
                    _, detail = msg
                    self._on_run_failed(detail)
        except queue.Empty:
            pass
        self.root.after(QUEUE_POLL_MS, self._drain_queue)

    def _apply_result(self, index: int, result: CheckResult) -> None:
        if 0 <= index < len(self._results):
            self._results[index] = result
        detail = result.reason or ""
        self.tree.item(
            str(index),
            values=(result.username, result.status.upper(), detail),
            tags=(result.status,),
        )
        self._completed += 1
        self.progress.configure(value=self._completed)
        self.status_var.set(f"Checking {self._completed}/{self._total}...")

    def _on_run_finished(self, results: List[CheckResult]) -> None:
        self.check_button.state(["!disabled"])
        self._worker = None
        self.status_var.set(f"Done. Checked {self._total} username(s).")

        if self.autosave_var.get():
            path = self.output_path_var.get().strip() or DEFAULT_OUTPUT_FILE
            try:
                written = save_results(results, path)
            except OSError as exc:
                messagebox.showerror("Could not save", str(exc))
                return
            if written:
                self.status_var.set(
                    f"Done. Saved {written} result(s) to {path}"
                )

    def _on_run_failed(self, detail: str) -> None:
        self.check_button.state(["!disabled"])
        self._worker = None
        self.status_var.set(f"Run failed: {detail}")
        messagebox.showerror("Check failed", detail)


def main() -> int:
    root = Tk()
    CheckerGUI(root)
    root.mainloop()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
