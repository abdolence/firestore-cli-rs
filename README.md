[![Cargo](https://img.shields.io/crates/v/firestore-cli.svg)](https://crates.io/crates/firestore-cli)

# fsq

A command line client for Google Firestore: a SQL REPL with tables, query plans and index statements,
and a scripting mode with JSON output. It is built on the [firestore](https://github.com/abdolence/firestore-rs)
crate, which translates SQL into Firestore pipelines.

## Features

* **SQL REPL:** highlighting, completion of collections and fields, statements across several lines and a history;
* **Results as tables:** nested values in compact cells, and an expanded view with one block per document;
* **Query plans:** `EXPLAIN` draws the plan as a coloured tree, with a warning when it reads every document of the
  collection;
* **Index statements:** `CREATE INDEX`, `DROP INDEX`, `ALTER COLLECTION` and `SHOW INDEXES`, waiting for an index
  build with a spinner;
* **Collections and fields:** `\l` lists the collections, and `\d` lists the field paths of a collection with their
  types, how often they are present and an example value;
* **Scripting:** a statement or a script without the REPL, with `--format json` or `jsonl` output for other tools.

## Installation

fsq builds against the firestore crate in a checkout of firestore-rs next to this repository:

```sh
git clone https://github.com/abdolence/firestore-rs.git
git clone https://github.com/abdolence/firestore-cli-rs.git
cargo install --path firestore-cli-rs
```

## Connecting

```sh
fsq -p my-project -d my-database
```

* `-p`, `--project`: the Google Cloud project. Without it fsq uses the project the firestore crate detects: the first
  of the `GCP_PROJECT`, `PROJECT_ID`, `GCP_PROJECT_ID` and `GOOGLE_CLOUD_PROJECT` environment variables that is set,
  then the project the Application Default Credentials file names, then the project of the metadata server. The
  project `gcloud config set project` sets is not read;
* `-d`, `--database`: the database ID, `(default)` by default;
* `FIRESTORE_EMULATOR_HOST`: connects to the Firestore emulator at that address instead.

fsq authenticates with Application Default Credentials (ADC), the credentials Google client libraries look up
on their own: the file `GOOGLE_APPLICATION_CREDENTIALS` names, the file `gcloud auth application-default login`
writes, or the metadata server on Google Cloud. `gcloud auth login` authorises only the `gcloud` tool, not fsq.

The prompt shows `project/database (edition)`, and `\c <database>` connects to another database of the project.

## Editions

SQL needs a database of the Firestore Enterprise edition. On a Standard edition database the REPL says so when it
connects, and `SELECT` and the writes fail. `\l`, `\d`, `SHOW COLLECTIONS`, `SHOW INDEXES` and the index statements
work on both editions.

## The REPL

A statement ends with `;` and can span several lines. A line that starts with `\` is a meta-command:

| Command | What it does |
|---|---|
| `\l` | lists the collections under the root, as `SHOW COLLECTIONS` does |
| `\d <collection> [--sample N]` | lists the field paths of a collection, see [Describing a collection](#describing-a-collection) |
| `\x [on\|off\|auto]` | switches the expanded view; without an argument toggles it |
| `\c <database>` | connects to another database of the project |
| `\timing [on\|off]` | adds the round trip time and the execution stats to the footer |
| `\wait [on\|off]` | waits for `CREATE INDEX` and `ALTER COLLECTION` to finish, on by default |
| `\?` | lists the meta-commands |
| `\q` | quits, as Ctrl-D does |

Tab completes a collection after `FROM`, `JOIN`, `INTO`, `UPDATE`, `COLLECTION`, `\d` and the `ON` of
`CREATE INDEX`, a meta-command at the start of a line, and otherwise a keyword or a field of the rows and descriptions
shown so far. Ctrl-C stops a running statement; a write that reached Firestore before it may still apply. The history
is kept in the `fsq` directory of the platform's data directory, such as `~/.local/share/fsq` on Linux.

## Results

```
my-project/my-database (enterprise)> SELECT __name__, title, address, tags, published FROM books ORDER BY title;
╭────────────────┬──────────────────┬─────────────┬───────────┬──────────────────────╮
│ __name__       ┆ title            ┆ address     ┆ tags      ┆ published            │
╞════════════════╪══════════════════╪═════════════╪═══════════╪══════════════════════╡
│ books/children ┆ Children of Dune ┆             ┆           ┆                      │
│ books/dune     ┆ Dune             ┆ {city, zip} ┆ [3 items] ┆ 2023-11-14T22:13:20Z │
│ books/messiah  ┆ Dune Messiah     ┆             ┆           ┆                      │
╰────────────────┴──────────────────┴─────────────┴───────────┴──────────────────────╯
3 rows · 760 ms
```

* the columns are the union of the fields of the rows shown: `__name__` first when the rows have document names,
  then the columns in the order the statement selects them, then the other fields by name;
* a map shows its keys, such as `{city, zip}`, an array its length, such as `[3 items]`, and a string longer than
  40 characters is cut with `…`. A reference is shown relative to the database, as `__name__` is;
* an empty cell is a field the document lacks, and `NULL` is a field that holds null. Firestore keeps the two
  apart, see the [semantics](https://firestore-rust.abdolence.dev/editions/semantics.html) of pipelines;
* a statement without `LIMIT` shows a page that fits the terminal and asks before the next one. A statement with
  `LIMIT` shows all its rows, and `EXPLAIN` is never paged, since its plan arrives with the last row;
* the footer shows the rows, the time the statement took and its notes, such as `more rows remain`.

`\x` shows each document as a block, with nested values pretty-printed, and `\x auto` does so only for a table wider
than the terminal. A result taller than the terminal goes through `$PAGER`, or `less -RFX` when it is
unset; an empty `PAGER` turns the pager off.

## Writes and index statements

`INSERT`, `UPDATE` and `DELETE` run as typed, with no confirmation, and print how many documents they modified. The
index statements run too: fsq runs every statement with `allow_index_statements()` of the firestore crate.

`CREATE INDEX` and `ALTER COLLECTION` start a build that takes minutes even for an empty collection. fsq waits for it
with a spinner and the elapsed time. Ctrl-C stops the wait, not the build, and `SHOW INDEXES FROM <collection>`
shows its state afterwards. `--no-wait` and `\wait off` return as soon as the build starts.

```sql
CREATE INDEX ON books (genre, rating DESC);
SHOW INDEXES FROM books;
DROP INDEX IF EXISTS `CICAgJim14AK`;
```

The statements and their options are in [Index statements](https://firestore-rust.abdolence.dev/sql/indexes.html).

## Query plans

`EXPLAIN SELECT ..` shows the plan Firestore chose, without reading any document, and `EXPLAIN ANALYZE SELECT ..`
also returns the rows and what the run read. The plan is drawn as a tree, followed by a warning for a *table scan*:
a read of every document of the collection, instead of an index. The Firestore emulator returns no plan, and fsq says
so.

`\timing on` runs every `SELECT` with `ANALYZE` and adds to the footer the round trip time, the documents read and
the read units. A *read unit* is how the Enterprise edition bills reads: 4 KiB of the data a read processes.

```
2 rows · 189 ms · round trip 187 ms · 3 documents read · 1 read unit
```

## Describing a collection

```sh
fsq -p my-project -d my-database describe books --sample 100
```

`\d books` in the REPL lists the same: each field path of a sample of documents, nested maps included, with the
types it holds, the share of the sampled documents that have it and an example value. The sample is 500 documents
unless `--sample` sets another size. On an Enterprise edition database it is a random sample. On a Standard edition
database, on the emulator and when the edition cannot be read, it is the first documents by ID.

```
my-project/my-database (enterprise)> \d books
╭──────────────┬──────────────┬─────────┬──────────────────────╮
│ field        ┆ types        ┆ present ┆ example              │
╞══════════════╪══════════════╪═════════╪══════════════════════╡
│ address      ┆ map          ┆ 33.3%   ┆ {city, zip}          │
│ address.city ┆ string       ┆ 33.3%   ┆ London               │
│ address.zip  ┆ string       ┆ 33.3%   ┆ N1 9GU               │
│ genre        ┆ string       ┆ 66.7%   ┆ sf                   │
│ pages        ┆ integer      ┆ 33.3%   ┆ 412                  │
│ published    ┆ timestamp    ┆ 33.3%   ┆ 2023-11-14T22:13:20Z │
│ rating       ┆ double       ┆ 100.0%  ┆ 4.5                  │
│ subtitle     ┆ null, string ┆ 66.7%   ┆ The second book      │
│ tags         ┆ array        ┆ 33.3%   ┆ [3 items]            │
│ title        ┆ string       ┆ 100.0%  ┆ Dune                 │
╰──────────────┴──────────────┴─────────┴──────────────────────╯
10 field paths · 753 ms · from a sample of 3 documents
```

## Scripting

```sh
fsq -p my-project -d my-database sql -e 'SELECT title FROM books LIMIT 10'
fsq -p my-project -d my-database sql -f report.sql
fsq -p my-project -d my-database --format jsonl sql -e 'SELECT * FROM books' | jq .title
echo 'SHOW COLLECTIONS; \d books' | fsq -p my-project -d my-database
```

* a script is split on `;` and runs one statement at a time, since the firestore crate takes one statement per call.
  Meta-commands run in scripts too;
* `-e` and `-f` stop at the first statement that fails and exit with 1;
* input piped to fsq runs as the REPL runs typed input: a failed statement is reported and the rest still runs, and
  fsq exits with 1 when any failed;
* `--format json` prints each result as a JSON array of objects, and `--format jsonl` as JSON lines, one object per
  line. A timestamp is RFC 3339 text, bytes are base64, a reference is its path relative to the database and a geo
  point is an object with `latitude` and `longitude`. The footers are left out, and notices go to stderr.

## Licence

Apache Software License (ASL)

## Author

Abdulla Abdurakhmanov
