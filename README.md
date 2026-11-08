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
* **Index files:** `fsq index` plans, syncs, exports, lists, creates and deletes indexes from a
  `firestore.indexes.json`, the file of the Firebase CLI;
* **Collections and fields:** `\l` lists the collections, and `\d` lists the field paths of a collection with their
  types, how often they are present and an example value;
* **Fluent Rust code:** `fsq codegen` and `\gen` show the code of the firestore crate's fluent API for a `SELECT`, with
  a row struct typed from sample rows;
* **Export and import:** documents as JSON lines, in an encoding that reads back to the same Firestore types;
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
| `\l [--recursive [--depth N] [--sample K]]` | lists the collections under the root, as `SHOW COLLECTIONS` does; with `--recursive` also the subcollections, see [Listing subcollections](#listing-subcollections) |
| `\d <collection> [--sample N]` | lists the field paths of a collection, see [Describing a collection](#describing-a-collection) |
| `\x [on\|off\|auto]` | switches the expanded view; without an argument toggles it |
| `\c <database>` | connects to another database of the project |
| `\timing [on\|off]` | adds the round trip time and the execution stats to the footer |
| `\wait [on\|off]` | waits for `CREATE INDEX` and `ALTER COLLECTION` to finish, on by default |
| `\gen ['SELECT'] [--param NAME=VALUE] [--sample N] [--offline]` | shows the fluent Rust code of a `SELECT`, or of the last one that ran, see [Fluent Rust code](#fluent-rust-code) |
| `\?` | lists the meta-commands |
| `\q` | quits, as Ctrl-D does |

The arguments of `\l`, `\d` and `\gen` are split as a shell splits them, so text that holds spaces or quotes is
quoted. `\gen` takes its `SELECT` as one argument, and a JSON string for `--param` is quoted twice:

```
\gen 'SELECT title FROM books WHERE genre = @genre' --param 'genre="science fiction"'
```

A `SELECT` that holds single quotes goes in double quotes.

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
  40 characters is cut in the middle with `…`, so that its start and end show. A reference is shown relative to the
  database, as `__name__` is;
* an empty cell is a field the document lacks, and `NULL` is a field that holds null. Firestore keeps the two
  apart, see the [semantics](https://firestore-rust.abdolence.dev/editions/semantics.html) of pipelines;
* a table wider than the terminal is cut to fit it, the longest cells first, in the middle with `…`; a table that is piped is not cut;
* a statement that returns no documents prints the footer alone, `0 rows`, and `--format json` an empty array;
* a statement without `LIMIT` shows a page that fits the terminal and asks before the next one. A statement with
  `LIMIT` shows all its rows, and `EXPLAIN` is never paged, since its plan arrives with the last row;
* the footer shows the rows, the time the statement took and its notes, such as `more rows remain`.

`\x` shows each document as a block, with nested values pretty-printed, and `\x auto` does so only for a table wider
than the terminal. A result taller than the terminal goes through `$PAGER`, or `less -RFX` when it is
unset; an empty `PAGER` turns the pager off. When `LESS` is not set, fsq sets it to `FRX` for the pager alone, as git
does, so that `less` keeps the output on screen after `q`, does not page what fits on one screen and shows colours; a
`LESS` that is set, even to nothing, is left as it is.

A call that waits on Firestore for more than a moment shows a spinner on stderr, unless stderr is not a terminal or
the results are JSON written to a pipe.

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

## Indexes from a file

`fsq index` works on the `firestore.indexes.json` format of the Firebase CLI: the `indexes` and the
`fieldOverrides` of each collection group. Planning and syncing use the index management of the
firestore crate, which owns one collection group at a time, so fsq plans and syncs the groups of a file
one after another.

```sh
fsq -p my-project index export -o firestore.indexes.json
fsq -p my-project index plan firestore.indexes.json
fsq -p my-project index sync firestore.indexes.json
fsq -p my-project index list --group books
fsq -p my-project index create books genre:asc rating:desc
fsq -p my-project index delete books genre:asc rating:desc --if-exists
```

* `plan <file> [--prune]` prints one row per index, field override and TTL field: `create`, `update`, `enable`,
  `delete`, `revert`, `disable`, `unchanged`, `pending`, `reverting`, `needs repair`, `undeclared`, `withheld` and
  `unrecognised`. It writes nothing;
* `sync <file> [--prune] [--no-wait]` applies the plan, with a spinner for each collection group while its builds
  run, and prints the same table. `--no-wait` returns once the last change is requested. Ctrl-C stops the wait, not
  the builds, and `fsq index list` shows their state afterwards;
* `list [--group <collection group>]` lists the live indexes and field overrides, as `SHOW INDEXES` does;
* `export [-o <file>]` writes the live indexes, field overrides and TTL fields as an indexes file, to stdout
  without `-o`. Items the firestore crate cannot represent are named on stderr and left out;
* `create <collection group> <field>:<mode>... [--scope collection|group] [--if-not-exists]` creates one composite
  index. The mode is `asc`, `desc` or `contains`;
* `delete <index id> [--if-exists]` or `delete <collection group> <field>:<mode>... [--scope ..] [--if-exists]`
  deletes one index, named by its ID or by its definition.

Without `--prune`, what the database has and the file does not declare is shown as `undeclared` and left alone,
in every collection group of the database, including the ones the file does not name. `--prune` deletes
undeclared indexes, reverts undeclared field overrides to automatic indexing and disables undeclared TTL fields.
A pruning sync deletes only after the indexes it created have finished building, even with `--no-wait`.

### The file

The file is the format of `firebase firestore:indexes` and `firebase deploy`, and `fsq index export` writes what
`firebase firestore:indexes` writes: `indexes`, with `collectionGroup`, `queryScope`, `fields` and `density`, and
`fieldOverrides`, with `ttl` and `indexes`. A field sets exactly one of `order`, `arrayConfig`, `searchConfig`
(a `textSpec` or a `geoSpec`) and `vectorConfig`. On a field override, an empty `indexes` exempts the field, the
`fieldPath` `*` names every field of the collection group, and `ttl: true` enables TTL on the field. `ttl: false`
declares nothing.

`density`, `unique`, `multikey` and `apiScope` are read from an index. The firestore crate declares the default
`apiScope`, `ANY_API`, and `multikey: false` only, so a file that sets another value is refused.

The Firebase CLI does not read the `fsq` keys. They hold what the Firebase format cannot carry:

| Key | Holds | Why it is not a Firebase key |
|---|---|---|
| `fsq.search` on an index | `textLanguage` and `textLanguageOverrideFieldPath` of a search index | the format has no key for the language options of a search index |
| `fsq.unique` on an index | `true` for a unique index | the Firebase CLI refuses a file whose index has `unique`; fsq reads that key too and writes its own |
| `fsq.ttl` on the file | a list of `{ "collectionGroup", "fieldPath" }`, a TTL field whose indexes are inherited | the format writes `ttl` on a field override only, and an override with no `indexes` exempts the field |

```json
{
  "indexes": [
    {
      "collectionGroup": "articles",
      "queryScope": "COLLECTION",
      "fields": [
        {
          "fieldPath": "body",
          "searchConfig": {
            "textSpec": { "indexSpecs": [{ "indexType": "TOKENIZED", "matchType": "MATCH_GLOBALLY" }] }
          }
        },
        { "fieldPath": "published", "order": "DESCENDING" }
      ],
      "density": "SPARSE_ANY",
      "fsq": { "search": { "textLanguage": "en" } }
    }
  ],
  "fieldOverrides": [
    { "collectionGroup": "sessions", "fieldPath": "expires_at", "ttl": true, "indexes": [] }
  ]
}
```

A key that neither the Firebase format nor `fsq` defines is refused with its path, so that no setting is dropped
without notice. A key set to `null` counts as absent. `export` lists on stderr the indexes and field overrides that
the firestore crate cannot represent, such as an index with `multikey`, and leaves them out of the file.

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

## Fluent Rust code

```sh
fsq -p my-project -d my-database codegen \
  'SELECT title, rating FROM books WHERE rating >= @min LIMIT 10' --param min=4.0
```

`codegen` prints the Rust code that the `codegen` feature of the firestore crate generates for a `SELECT`: a row
struct, and an async function that runs the equivalent pipeline of the fluent API. It is meant for porting a
statement to application code, and the output is a starting point to paste and edit. fsq prints what the crate
returns and generates nothing itself. `\gen '<SELECT>'` in the REPL does the same, and `\gen` alone uses the last
`SELECT` that ran.

| Option | What it does |
|---|---|
| `--param name=value` | binds `@name` to a JSON literal, which gives the function argument its type: `4` is an integer, `4.0` a double, `true` a boolean and `"text"` a string. In a shell the quotes of a string need quoting too, as in `--param 'genre="sf"'`. A `?` parameter cannot be bound |
| `--sample N` | runs the `SELECT` for at most N rows, 100 by default, and types the fields of the row struct from them. The statement runs as a read, with a limit of N rows, and the output does not show that limit |
| `--offline` | does not run the `SELECT` and needs no connection, so every field is typed as unknown JSON; it does not combine with `--sample` |

A statement that is not a `SELECT`, and a `SELECT` with a construct the crate cannot show, fails with the crate's
error that names the construct. The [fluent code](https://firestore-rust.abdolence.dev/sql/codegen.html) chapter of
the crate lists what the output covers and how it types the fields. The sample is read with a `SELECT`, so it needs
an Enterprise edition database, and `--offline` works on any edition.

## Listing subcollections

`\l --recursive` lists the collections under the root and the subcollections below them:

```
my-project/my-database (enterprise)> \l --recursive --depth 3
╭────────────┬───────────────────────────┬───────┬──────────────────────────╮
│ collection ┆ path                      ┆ depth ┆ seen_under               │
╞════════════╪═══════════════════════════╪═══════╪══════════════════════════╡
│ books      ┆ books                     ┆ 1     ┆                          │
│ users      ┆ users                     ┆ 1     ┆                          │
│ addresses  ┆ users/*/addresses         ┆ 2     ┆ 2 of 3 sampled users     │
│ orders     ┆ users/*/orders            ┆ 2     ┆ 1 of 3 sampled users     │
│ lines      ┆ users/*/addresses/*/lines ┆ 3     ┆ 1 of 2 sampled addresses │
╰────────────┴───────────────────────────┴───────┴──────────────────────────╯
5 collections · 3.10 s · sampled 20 docs per collection
```

Firestore lists the collections under one document at a time, so no call returns every subcollection.
`\l --recursive` reads a sample of the documents of each collection and lists the collections under them:

* `--sample K` is the number of documents read per collection path, 20 by default. For `users/*/addresses` that is 20
  documents in all, taken from the `addresses` collections of the sampled users;
* `--depth N` is the number of levels listed, 2 by default. Depth 1 is the root collections alone, and depth 3 reads
  the documents of the collections at depths 1 and 2;
* the sample is the first documents by ID. It includes a document that does not exist and has subcollections, such
  as `users/carol` when only `users/carol/orders/o1` was written;
* `path` shows each document ID on the way as `*`, `depth` is the level, and `seen_under` is how many of the sampled
  documents of the collection above have this one. A subcollection that only documents outside the sample have is
  not listed;
* the footer gives the number of collections, the time and the sample. `--format json` and `jsonl` give the same
  rows, with `seen_under` as text, and no footer.

## Export and import

```sh
fsq -p my-project -d my-database export books -o books.jsonl
fsq -p my-project -d my-database export users --recursive -o users.jsonl
fsq -p my-project -d my-database import books.jsonl
```

`export` writes one document per line to stdout, or to the file `-o` names. A line is a JSON object with the
document's `__name__`, relative to the database, and its fields. The values use the tagged JSON encoding of the
firestore crate, which reads back to the same Firestore types: a timestamp is `{"$timestamp": "2023-11-14T22:13:20Z"}`,
bytes are `{"$bytes": ".."}`, a reference is `{"$ref": "books/dune"}` and an integer beyond 2^53 is `{"$int": ".."}`.
The [JSON conversion](https://firestore-rust.abdolence.dev/json.html) chapter of the crate lists every type.

| Option | What it does |
|---|---|
| `<collection>` | exports a collection, by path for a subcollection such as `users/alice/orders` |
| `--group` | exports every collection with that ID, at any depth; the argument is a collection ID, and `--recursive` does not combine with it |
| `--recursive` | adds the subcollections of every document, including those of a document that does not exist and has only subcollections; each name holds its full relative path. It lists the subcollections with one extra request per document, and writes no document that does not exist |
| `--sql '<SELECT>'` | exports the rows of a `SELECT` instead of a collection, on an Enterprise edition database. A row needs its `__name__` to be imported |
| `--with-metadata` | adds `__create_time__` and `__update_time__` to every document. `import` ignores both |
| `--plain` | writes the plain encoding, which is for reading and cannot be imported |
| `-o`, `--output` | writes to a file instead of stdout |

Listing a collection works on both editions. Unless the documents go to a terminal, a spinner on stderr shows how
many documents were written, and a line on stderr states the total.

`import` reads a file, or stdin for `-`, and writes the documents in batches through the batch writer of the
firestore crate. A batch holds at most 500 writes and about 9 MiB, and it ends before a document that it already
holds, since Firestore refuses a request that writes one document twice. A bar shows the position in the file, or a
spinner when the input is stdin.

| Option | What it does |
|---|---|
| `--on-conflict skip` | leaves a document that exists as it is and counts it as skipped |
| `--on-conflict fail` | reports a document that exists as a failure |
| `--merge` | writes the top-level fields a line lists, and keeps the other fields of an existing document; it cannot be combined with `--on-conflict` |
| `--keep-going` | goes on after a failure and reports every one |

Without `--on-conflict` or `--merge`, import creates a document, or replaces it when it exists, so that an export
imported again gives the same documents.

A line that is not valid JSON, or not a document in the tagged encoding, fails with its line number and the code of
the firestore crate, such as `JSON_UNKNOWN_WRAPPER`. A document that Firestore refuses fails with its line number and
the status code. A batch is not atomic: each write of it is counted as written, skipped or failed, so that the other
writes of a batch with a failure are applied. A request that Firestore refuses as a whole fails every write of it,
and the failure names the first and the last line of the batch. Without `--keep-going` the import stops at the first
failure, and with it the import goes on after a line, a write or a request fails. fsq exits with 1 when any document
failed. The last line on stderr counts the documents written, skipped and failed.

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
  line. The values use the plain JSON encoding of the firestore crate, which is for display and cannot be read back
  into the same Firestore types: a timestamp is RFC 3339 text, bytes are base64, a reference is its path relative to
  the database, a geo point is an object with `latitude` and `longitude`, a vector is an array of numbers, and `NaN`
  and the infinities are the strings `"NaN"`, `"Infinity"` and `"-Infinity"`. The footers are left out, and notices go
  to stderr.

## Licence

Apache Software License (ASL)

## Author

Abdulla Abdurakhmanov
