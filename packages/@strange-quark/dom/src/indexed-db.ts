// SPDX-License-Identifier: MIT

import { reflectedEvent } from "./document.js";
import { dispatchTrustedEvent, DOMException, Event, EventTarget } from "./events.js";
import { bufferBytes } from "./web-crypto.js";

type Key = number | string | Date | ArrayBuffer | Key[];
type Entries = [Key, unknown][];
type Database = { version: number; stores: Map<string, Entries> };
type Snapshot = { revision: number; database: Database | null; persistent: boolean };
type Mode = "readonly" | "readwrite" | "versionchange";
type DatabaseJob = { start(done: () => void): void; finishOnShutdown?(): void };
type Host = {
  cloneMessage<T>(data: T): T;
  serializeValue(data: unknown): Uint8Array;
  deserializeValue<T>(data: Uint8Array): T;
  idbRead(name: string): [number, Uint8Array | null] | null;
  idbCommit(name: string, revision: number, data: Uint8Array | null): boolean;
  idbShutdownTimeRemaining(): boolean;
  postTask(callback: () => void, afterMicrotasks?: () => void): void;
};

function error(name: string, message: string): DOMException { return new DOMException(message, name); }

function normalizeKey(value: unknown, ancestors = new Set<unknown>()): Key {
  if (typeof value === "string" || typeof value === "number" && !Number.isNaN(value)) return value;
  if (value instanceof Date && Number.isFinite(value.getTime())) return new Date(value.getTime());
  if (value instanceof ArrayBuffer || ArrayBuffer.isView(value)) {
    try { return new Uint8Array(bufferBytes(value)).buffer; } catch { /* detached or shared input */ }
  }
  if (Array.isArray(value) && !ancestors.has(value)) {
    ancestors.add(value);
    const pair = Array.from(value, item => normalizeKey(item, ancestors));
    ancestors.delete(value);
    return pair;
  }
  throw error("DataError", "Invalid IndexedDB key");
}

function order(value: Key): number {
  if (typeof value === "number") return 0;
  if (value instanceof Date) return 1;
  if (typeof value === "string") return 2;
  if (value instanceof ArrayBuffer) return 3;
  return 4;
}

function compare<T extends number | string>(a: T, b: T): number { return a === b ? 0 : a < b ? -1 : 1; }

function compareLists<T>(a: ArrayLike<T>, b: ArrayLike<T>, item: (a: T, b: T) => number): number {
  for (let i = 0; i < Math.min(a.length, b.length); i++) {
    const result = item(a[i], b[i]);
    if (result !== 0) return result;
  }
  return compare(a.length, b.length);
}

function compareKeys(a: Key, b: Key): number {
  const priority = order(a) - order(b);
  if (priority !== 0) return Math.sign(priority);
  if (typeof a === "number" || typeof a === "string") return compare(a, b as typeof a);
  if (a instanceof Date) return compare(a.getTime(), (b as Date).getTime());
  if (a instanceof ArrayBuffer) return compareLists(new Uint8Array(a), new Uint8Array(b as ArrayBuffer), compare);
  return compareLists(a, b as Key[], compareKeys);
}

function insertionPoint(entries: Entries, key: Key): number {
  let low = 0, high = entries.length;
  while (low < high) {
    const middle = low + ((high - low) >> 1);
    if (compareKeys(entries[middle][0], key) < 0) low = middle + 1;
    else high = middle;
  }
  return low;
}

class IDBVersionChangeEvent extends Event {
  readonly oldVersion: number;
  readonly newVersion: number | null;
  constructor(type: string, options: { oldVersion?: number; newVersion?: number | null } = {}) {
    super(type);
    this.oldVersion = Number(options.oldVersion ?? 0);
    this.newVersion = options.newVersion == null ? null : Number(options.newVersion);
  }
}

export function installIndexedDB(global: Record<PropertyKey, unknown>, host: Host): () => void {
  const jobs = new Map<string, DatabaseJob[]>();
  const volatile = new Map<string, [number, Uint8Array | null]>();
  const token = Symbol("IndexedDB constructor");
  function schedule(name: string, job: DatabaseJob): void {
    const queue = jobs.get(name) ?? [];
    jobs.set(name, queue);
    queue.push(job);
    if (queue.length !== 1) return;
    const done = () => {
      queue.shift();
      if (queue.length) host.postTask(() => queue[0].start(done));
      else jobs.delete(name);
    };
    host.postTask(() => queue[0].start(done));
  }
  function read(name: string): Snapshot {
    const disk = host.idbRead(name);
    const [revision, bytes] = disk ?? volatile.get(name) ?? [0, null];
    const database = bytes === null ? null : host.deserializeValue<Database>(bytes);
    if (bytes !== null && !(database?.stores instanceof Map)) throw error("UnknownError", "Invalid IndexedDB data");
    return { revision, database, persistent: disk !== null };
  }
  function persist(name: string, { revision, persistent }: Snapshot, database: Database | null): void {
    const bytes = database === null ? null : host.serializeValue(database);
    if (!persistent) volatile.set(name, [revision + 1, bytes]);
    else if (!host.idbCommit(name, revision, bytes)) throw error("AbortError", "IndexedDB changed in another document");
  }

  const setResult = Symbol("IDBRequest result");
  const setError = Symbol("IDBRequest error");
  const setPending = Symbol("IDBRequest pending");
  class IDBRequest extends EventTarget {
    #ready = false;
    #result: unknown;
    #error: DOMException | null = null;
    constructor(secret: typeof token, readonly source: IDBObjectStore | null = null, readonly transaction: IDBTransaction | null = null) {
      if (secret !== token) throw new TypeError("Illegal constructor");
      super();
    }
    get readyState(): "pending" | "done" { return this.#ready ? "done" : "pending"; }
    get result(): unknown {
      if (!this.#ready) throw error("InvalidStateError", "IndexedDB request is pending");
      return this.#result;
    }
    get error(): DOMException | null {
      if (!this.#ready) throw error("InvalidStateError", "IndexedDB request is pending");
      return this.#error;
    }
    [setResult](value: unknown): void { this.#ready = true; this.#result = value; }
    [setError](failure: DOMException): void { this.#ready = true; this.#error = failure; }
    [setPending](): void { this.#ready = false; this.#result = undefined; this.#error = null; }
  }
  class IDBOpenDBRequest extends IDBRequest {}
  function success(request: IDBRequest, value: unknown): void {
    request[setResult](value);
    dispatchTrustedEvent(request, new Event("success"));
  }
  function failure(request: IDBRequest, cause: unknown): boolean {
    request[setError](cause instanceof DOMException ? cause : error("UnknownError", String(cause)));
    return dispatchTrustedEvent(request, new Event("error", { bubbles: true, cancelable: true }));
  }

  const enqueue = Symbol("queue request");
  const begin = Symbol("begin transaction");
  const finishOnShutdown = Symbol("finish committing transaction");
  const upgrade = Symbol("schema upgrade");
  type Operation = { source: IDBObjectStore; request: IDBRequest; run: (entries: Entries) => unknown };
  class IDBTransaction extends EventTarget {
    #pending: Operation[] = [];
    #closed = false;
    #error: DOMException | null = null;
    #writes = false;
    #aborted = false;
    #committing = false;
    #snapshot?: Snapshot;
    #stores = new Map<string, IDBObjectStore>();
    constructor(secret: typeof token, readonly db: IDBDatabase, readonly mode: Mode, readonly objectStoreNames: string[]) {
      if (secret !== token) throw new TypeError("Illegal constructor");
      super();
    }
    get error(): DOMException | null { return this.#error; }
    objectStore(name: string): IDBObjectStore {
      name = String(name);
      if (this.#closed) throw error("InvalidStateError", "Transaction has finished");
      if (!this.objectStoreNames.includes(name)) throw error("NotFoundError", "Object store does not exist");
      let store = this.#stores.get(name);
      if (store === undefined) this.#stores.set(name, store = new IDBObjectStore(token, name, this));
      return store;
    }
    abort(): void {
      if (this.#committing || this.#closed || this.#aborted) throw error("InvalidStateError", "Transaction is finishing");
      this.#aborted = true;
    }
    commit(): void {
      if (this.#committing || this.#closed || this.#aborted) throw error("InvalidStateError", "Transaction is finishing");
      this.#committing = true;
    }
    [enqueue](source: IDBObjectStore, run: (entries: Entries) => unknown, writes = false,
      request = new IDBRequest(token, source, this)): IDBRequest {
      if (this.#committing || this.#closed || this.#aborted) throw error("TransactionInactiveError", "Transaction is inactive");
      if (writes && this.mode === "readonly") throw error("ReadOnlyError", "Transaction is read-only");
      request[setPending]();
      this.#pending.push({ source, request, run });
      this.#writes ||= writes;
      return request;
    }
    [finishOnShutdown](readSnapshot: () => Snapshot): void {
      if (this.#closed) return;
      this.#closed = true;
      if (!this.#committing || this.#aborted || (!this.#writes && this.mode !== "versionchange")) return;
      const snapshot = this.#snapshot ?? readSnapshot();
      if (snapshot.database === null) return;
      for (const item of this.#pending) {
        if (!host.idbShutdownTimeRemaining()) return;
        this.#execute(item, snapshot.database);
      }
      if (host.idbShutdownTimeRemaining()) persist(this.db.name, snapshot, snapshot.database);
    }

    #execute(item: Operation, database: Database): unknown {
      const entries = database.stores.get(item.source.name);
      if (entries === undefined) throw error("NotFoundError", "Object store no longer exists");
      return item.run(entries);
    }

    [begin](snapshot: Snapshot, done: () => void, completion?: () => void): void {
      this.#snapshot = snapshot;
      const abort = (cause: unknown) => {
        this.#closed = true;
        this.#snapshot = undefined;
        this.#error = cause instanceof DOMException ? cause : error("AbortError", String(cause));
        for (const pending of this.#pending.splice(0)) failure(pending.request, this.#error);
        dispatchTrustedEvent(this, new Event("abort", { bubbles: true }));
        done();
      };
      const task = () => {
        if (this.#aborted || snapshot.database === null) {
          abort(error("AbortError", "Transaction was aborted"));
          return;
        }
        const item = this.#pending[0];
        if (item !== undefined) {
          let value: unknown;
          try {
            value = this.#execute(item, snapshot.database);
          } catch (cause) {
            this.#pending.shift();
            if (failure(item.request, cause)) abort(cause);
            return;
          }
          // Keep the operation pending until its effect is in the snapshot.
          // If V8 is interrupted during a write, the drain can replay it.
          this.#pending.shift();
          success(item.request, value);
          return;
        }
        this.#committing = true;
        try {
          if (this.mode === "versionchange" || this.#writes) persist(this.db.name, snapshot, snapshot.database);
        } catch (cause) {
          abort(cause);
          return;
        }
        this.#closed = true;
        this.#snapshot = undefined;
        dispatchTrustedEvent(this, new Event("complete"));
        completion?.();
        done();
      };
      const advance = () => {
        if (this.#closed) return;
        if (!this.#aborted && this.#pending.length === 0) this.#committing = true;
        host.postTask(task, advance);
      };
      host.postTask(task, advance);
    }
  }

  const stepCursor = Symbol("step cursor");
  const cursorValue = Symbol("cursor value");
  type Direction = "next" | "nextunique" | "prev" | "prevunique";

  class IDBCursor {
    #position?: Key;
    #key?: Key;
    #ready = false;
    [cursorValue]: unknown;

    constructor(secret: typeof token, readonly source: IDBObjectStore,
      readonly direction: Direction, readonly request: IDBRequest, private readonly filter?: Key) {
      if (secret !== token) throw new TypeError("Illegal constructor");
    }
    get key(): Key | undefined { return this.#key; }
    get primaryKey(): Key | undefined { return this.#key; }

    continue(key?: unknown): void {
      if (!this.#ready) throw error("InvalidStateError", "The cursor has no current value");
      const next = key === undefined ? undefined : normalizeKey(key);
      if (next !== undefined && compareKeys(next, this.#position!) * this.#sign() <= 0) {
        throw error("DataError", "The cursor must advance in its direction");
      }
      this[stepCursor](next);
    }

    advance(count: number): void {
      count = Number(count);
      if (!Number.isInteger(count) || count < 1 || count > 0xffff_ffff) throw new TypeError("Invalid cursor count");
      if (!this.#ready) throw error("InvalidStateError", "The cursor has no current value");
      this[stepCursor](undefined, count);
    }

    #sign(): number { return this.direction.startsWith("next") ? 1 : -1; }

    [stepCursor](seek?: Key, count = 1): void {
      this.source.transaction[enqueue](this.source, entries => {
        const sign = this.#sign();
        const bound = seek ?? this.#position ?? this.filter;
        let index = sign === 1 ? 0 : entries.length - 1;
        if (bound !== undefined) {
          index = insertionPoint(entries, bound);
          const equal = entries[index] !== undefined && compareKeys(entries[index][0], bound) === 0;
          const exclusive = seek === undefined && this.#position !== undefined;
          if (sign === 1 && equal && exclusive) index++;
          if (sign === -1 && (!equal || exclusive)) index--;
        }
        const item = entries[index + (count - 1) * sign];
        if (item === undefined || this.filter !== undefined && compareKeys(this.filter, item[0]) !== 0) {
          return null;
        }
        this.#position = item[0];
        [this.#key, this[cursorValue]] = host.cloneMessage(item);
        this.#ready = true;
        return this;
      }, false, this.request);
      this.#ready = false;
    }
  }

  class IDBCursorWithValue extends IDBCursor {
    get value(): unknown { return this[cursorValue]; }
  }

  class IDBObjectStore {
    constructor(secret: typeof token, readonly name: string, readonly transaction: IDBTransaction) {
      if (secret !== token) throw new TypeError("Illegal constructor");
    }
    get(key: unknown): IDBRequest {
      const normalized = normalizeKey(key);
      return this.transaction[enqueue](this, entries => {
        const item = entries[insertionPoint(entries, normalized)];
        return item !== undefined && compareKeys(item[0], normalized) === 0 ? host.cloneMessage(item[1]) : undefined;
      });
    }
    put(value: unknown, key: unknown): IDBRequest {
      const normalized = normalizeKey(key);
      const clone = host.cloneMessage(value);
      return this.transaction[enqueue](this, entries => {
        const index = insertionPoint(entries, normalized);
        const existing = entries[index];
        entries.splice(index, existing !== undefined && compareKeys(existing[0], normalized) === 0 ? 1 : 0, [normalized, clone]);
        return host.cloneMessage(normalized);
      }, true);
    }
    delete(key: unknown): IDBRequest {
      const normalized = normalizeKey(key);
      return this.transaction[enqueue](this, entries => {
        const i = insertionPoint(entries, normalized);
        if (entries[i] !== undefined && compareKeys(entries[i][0], normalized) === 0) entries.splice(i, 1);
      }, true);
    }
    clear(): IDBRequest { return this.transaction[enqueue](this, entries => { entries.length = 0; }, true); }
    openCursor(key?: unknown, direction: Direction = "next"): IDBRequest {
      if (!["next", "nextunique", "prev", "prevunique"].includes(direction)) throw new TypeError("Invalid cursor direction");
      const filter = key == null ? undefined : normalizeKey(key);
      const request = new IDBRequest(token, this, this.transaction);
      new IDBCursorWithValue(token, this, direction, request, filter)[stepCursor]();
      return request;
    }
    getAll(key?: unknown, count?: number): IDBRequest { return this.#getAll(1, key, count); }
    getAllKeys(key?: unknown, count?: number): IDBRequest { return this.#getAll(0, key, count); }
    #getAll(index: 0 | 1, key?: unknown, count?: number): IDBRequest {
      const filter = key == null ? undefined : normalizeKey(key);
      const limit = count === undefined ? 0 : Number(count);
      if (!Number.isInteger(limit) || limit < 0 || limit > 0xffff_ffff) throw new TypeError("Invalid IndexedDB count");
      return this.transaction[enqueue](this, entries => {
        const selected = filter === undefined ? entries : entries.filter(([key]) => compareKeys(key, filter) === 0);
        return host.cloneMessage(selected.slice(0, limit || selected.length).map(entry => entry[index]));
      });
    }
  }

  class IDBDatabase extends EventTarget {
    readonly objectStoreNames: string[];
    #closed = false;
    [upgrade]: { database: Database; transaction: IDBTransaction } | null = null;
    constructor(secret: typeof token, readonly name: string, database: Database) {
      if (secret !== token) throw new TypeError("Illegal constructor");
      super();
      this.version = database.version;
      this.objectStoreNames = [...database.stores.keys()].sort();
    }
    readonly version: number;

    createObjectStore(name: string, options: { keyPath?: unknown; autoIncrement?: boolean } = {}): IDBObjectStore {
      name = String(name);
      const pending = this[upgrade];
      if (pending === null) throw error("InvalidStateError", "Object stores can only be created during an upgrade");
      if (pending.database.stores.has(name)) throw error("ConstraintError", "Object store already exists");
      if (options?.keyPath != null || options?.autoIncrement) throw error("NotSupportedError", "Expected an out-of-line key");
      pending.database.stores.set(name, []);
      this.objectStoreNames.push(name);
      this.objectStoreNames.sort();
      return pending.transaction.objectStore(name);
    }
    transaction(storeNames: string | Iterable<string>, mode: Mode = "readonly"): IDBTransaction {
      if (this.#closed || this[upgrade] !== null) throw error("InvalidStateError", "Database is not available for transactions");
      if (mode !== "readonly" && mode !== "readwrite") throw new TypeError("Invalid IndexedDB transaction mode");
      const names = [...new Set(typeof storeNames === "string" ? [storeNames] : Array.from(storeNames, String))].sort();
      if (names.length === 0) throw error("InvalidAccessError", "No object stores in the transaction");
      if (!names.every(name => this.objectStoreNames.includes(name))) throw error("NotFoundError", "Object store does not exist");
      const transaction = new IDBTransaction(token, this, mode, names);
      schedule(this.name, {
        start: done => {
          try {
            transaction[begin](read(this.name), done);
          } catch {
            transaction[begin]({ revision: 0, database: null, persistent: false }, done);
          }
        },
        finishOnShutdown: () => transaction[finishOnShutdown](() => read(this.name)),
      });
      return transaction;
    }
    close(): void { this.#closed = true; }
  }

  class IDBFactory {
    constructor(secret: typeof token) { if (secret !== token) throw new TypeError("Illegal constructor"); }
    cmp(a: unknown, b: unknown): number { return compareKeys(normalizeKey(a), normalizeKey(b)); }
    open(name: string, version?: number): IDBOpenDBRequest {
      if (arguments.length === 0) throw new TypeError("Expected IndexedDB database name");
      name = String(name);
      if (version !== undefined) {
        version = Math.trunc(Number(version));
        if (!Number.isSafeInteger(version) || version < 1) throw new TypeError("Invalid IndexedDB version");
      }
      const request = new IDBOpenDBRequest(token);
      const job: DatabaseJob = {
        start: done => {
          try {
            const snapshot = read(name);
            const old = snapshot.database?.version ?? 0;
            const next = version ?? (old || 1);
            if (old > next) throw error("VersionError", "The database has a newer version");
            const database = { version: next, stores: snapshot.database?.stores ?? new Map<string, Entries>() };
            const db = new IDBDatabase(token, name, database);
            if (old === next) {
              success(request, db);
              done();
              return;
            }
            const upgraded = new IDBTransaction(token, db, "versionchange", db.objectStoreNames);
            job.finishOnShutdown = () => upgraded[finishOnShutdown](() => ({ ...snapshot, database }));
            db[upgrade] = { database, transaction: upgraded };
            request[setResult](db);
            Object.defineProperty(request, "transaction", { configurable: true, value: upgraded });
            upgraded.addEventListener("abort", () => {
              Object.defineProperty(request, "transaction", { value: null });
              failure(request, upgraded.error);
            }, { once: true });
            dispatchTrustedEvent(request, new IDBVersionChangeEvent("upgradeneeded", { oldVersion: old, newVersion: next }));
            upgraded[begin]({ ...snapshot, database }, done, () => {
              db[upgrade] = null;
              Object.defineProperty(request, "transaction", { value: null });
              success(request, db);
            });
          } catch (cause) {
            failure(request, cause);
            done();
          }
        },
      };
      schedule(name, job);
      return request;
    }
    deleteDatabase(name: string): IDBOpenDBRequest {
      name = String(name);
      const request = new IDBOpenDBRequest(token);
      schedule(name, {
        start: done => {
          try {
            const snapshot = read(name);
            if (snapshot.database !== null) persist(name, snapshot, null);
            request[setResult](undefined);
            dispatchTrustedEvent(request, new IDBVersionChangeEvent("success", { oldVersion: snapshot.database?.version ?? 0 }));
          } catch (cause) {
            failure(request, cause);
          }
          done();
        },
      });
      return request;
    }
  }

  for (const [Type, events] of [
    [IDBRequest, ["success", "error"]], [IDBOpenDBRequest, ["upgradeneeded", "blocked"]],
    [IDBTransaction, ["complete", "abort", "error"]], [IDBDatabase, ["close", "versionchange", "error", "abort"]],
  ] as const) {
    for (const event of events) reflectedEvent(Type.prototype, event);
  }
  Object.assign(global, { indexedDB: new IDBFactory(token), IDBFactory, IDBRequest, IDBOpenDBRequest,
    IDBDatabase, IDBTransaction, IDBObjectStore, IDBCursor, IDBCursorWithValue, IDBVersionChangeEvent });
  return () => {
    for (const queue of jobs.values()) {
      for (const job of queue) {
        if (!host.idbShutdownTimeRemaining()) return;
        try { job.finishOnShutdown?.(); } catch { /* Failed transactions roll back. */ }
      }
    }
  };
}
