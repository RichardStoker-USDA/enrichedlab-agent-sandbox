# Working copies

Each attempt of an analysis has two run volumes in its tenant: a checksummed copy of its input and the
volume its outputs were written to. They serve custody while the analysis runs and recovery after it. The results,
the project package, lineage, people's reviews and the kept decision texts are the scientific record and never
depend on them.

The run volumes are kept until the retention date set when the analysis was prepared, then cleared by a site-owned
cleanup job. The job clears a volume only after that date,
when no Pod that is still running names it, and when the broker records the analysis as finished with retained result
files or as failed or cancelled. It never touches source volumes or the broker's own volumes.

Migration 033 keeps what the sweep observed in `broker_task_working_copies`: each run volume with its analysis, role
and retention date, and, once cleared, the time and the SHA-256 of the sweep's receipt for it. A cleared volume stays
cleared and never moves to another analysis or role. The sweep reads analysis state with
`broker_working_copy_tasks` and records with `broker_record_working_copies`; it calls both as the migration role, and
no runtime role can call either or read the table.

A project reader reads `GET /api/v1/projects/{project}/working-copies` (`broker_project_working_copies`): for each
analysis the sweep has seen, `kept_until` (the later of its volumes' dates), `cleared_at` once every volume is gone,
and the number of volumes. The API stays out of service until migration 033 and that grant exist. The analysis page
says it in one line under "All files": "Working copy kept until October 25 (in 30 days). Results and the package
stay." or "Working copy cleared October 26. Results and the package stay." An analysis the sweep has not seen yet
shows no line.

Each file in that list says in plain words what it is for, from its path and reader kind alone
(`clients/workspace/src/file_role.ts`): a results table, a copy of the input as the analysis read it, the code that
ran, the run's log. The path, type and SHA-256 stay in the file record below the reader.

PostgreSQL checks: `scripts/qualify_remote_broker.sh working-copies`.
