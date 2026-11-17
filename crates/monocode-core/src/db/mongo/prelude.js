// The MongoDB console's shell: a small mongosh-like surface evaluated in a
// sandboxed QuickJS context. Nothing here touches the network. A statement
// such as `db.users.find({ age: { $gt: 30 } }).sort({ name: 1 })` evaluates
// to a *plan* (which database, collection, method, arguments and cursor
// modifiers), serialised as Extended JSON; Rust runs the plan with the driver.
// BSON helpers (ObjectId, ISODate, NumberLong…) serialise to their Extended
// JSON form, so types survive the trip.
(function (global) {
  "use strict";
  const PLAN = "__monocode_plan";

  const hex = (n, width) => n.toString(16).padStart(width, "0");
  const randomHex = (bytes) => {
    let out = "";
    for (let i = 0; i < bytes; i++) out += hex(Math.floor(Math.random() * 256), 2);
    return out;
  };
  const B64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  const hexToBase64 = (h) => {
    const bytes = [];
    for (let i = 0; i < h.length; i += 2) bytes.push(parseInt(h.slice(i, i + 2), 16));
    let out = "";
    for (let i = 0; i < bytes.length; i += 3) {
      const [a, b, c] = [bytes[i], bytes[i + 1], bytes[i + 2]];
      const n = (a << 16) | ((b || 0) << 8) | (c || 0);
      out += B64[(n >> 18) & 63] + B64[(n >> 12) & 63];
      out += b === undefined ? "=" : B64[(n >> 6) & 63];
      out += c === undefined ? "=" : B64[n & 63];
    }
    return out;
  };
  const fail = (message) => {
    throw new TypeError(message);
  };

  // ---- BSON helpers: callable with or without `new`, as in mongosh ----

  function ObjectId(id) {
    if (!(this instanceof ObjectId)) return new ObjectId(id);
    if (id instanceof ObjectId) id = id.$oid;
    if (id === undefined || id === null) {
      id = hex(Math.floor(Date.now() / 1000), 8) + randomHex(8);
    }
    if (typeof id !== "string" || !/^[0-9a-fA-F]{24}$/.test(id)) {
      fail("ObjectId() takes 24 hexadecimal characters");
    }
    Object.defineProperty(this, "$oid", { value: id.toLowerCase() });
  }
  ObjectId.prototype.toHexString = function () {
    return this.$oid;
  };
  ObjectId.prototype.toString = function () {
    return this.$oid;
  };
  ObjectId.prototype.getTimestamp = function () {
    return new Date(parseInt(this.$oid.slice(0, 8), 16) * 1000);
  };
  ObjectId.prototype.equals = function (other) {
    return other instanceof ObjectId && other.$oid === this.$oid;
  };
  ObjectId.prototype.toJSON = function () {
    return { $oid: this.$oid };
  };
  ObjectId.createFromHexString = (h) => new ObjectId(h);
  ObjectId.createFromTime = (seconds) => new ObjectId(hex(Math.floor(seconds), 8) + "0".repeat(16));

  function ISODate(text) {
    const date = text === undefined ? new Date() : new Date(text);
    if (isNaN(date.getTime())) fail(`ISODate(): ${JSON.stringify(text)} is not a date`);
    return date;
  }

  // Each tagged value keeps its Extended JSON form and prints like mongosh.
  function tagged(json, text) {
    const value = Object.create(null);
    Object.defineProperty(value, "toJSON", { value: () => json });
    Object.defineProperty(value, "toString", { value: () => text });
    return value;
  }

  function integer(v, what) {
    const text = typeof v === "string" ? v.trim() : String(Math.trunc(Number(v)));
    if (!/^-?\d+$/.test(text)) fail(`${what}() takes an integer`);
    return text;
  }

  const NumberLong = (v = 0) => {
    const n = integer(v, "NumberLong");
    return tagged({ $numberLong: n }, `Long("${n}")`);
  };
  const NumberInt = (v = 0) => {
    const n = String(Number(integer(v, "NumberInt")) | 0);
    return tagged({ $numberInt: n }, `Int32(${n})`);
  };
  const NumberDecimal = (v = "0") => {
    const n = String(v);
    return tagged({ $numberDecimal: n }, `Decimal128("${n}")`);
  };
  const Double = (v = 0) => {
    const n = Number(v);
    const text = Number.isFinite(n) ? String(n) : n > 0 ? "Infinity" : n < 0 ? "-Infinity" : "NaN";
    const json = Number.isInteger(n) && Math.abs(n) < 1e21 ? `${text}.0` : text;
    return tagged({ $numberDouble: json }, `Double(${text})`);
  };
  const Timestamp = (t = 0, i = 0) => {
    if (typeof t === "object" && t !== null) ({ t = 0, i = 0 } = t);
    return tagged({ $timestamp: { t: Number(t) >>> 0, i: Number(i) >>> 0 } }, `Timestamp({ t: ${t}, i: ${i} })`);
  };
  const BinData = (subType, base64) =>
    tagged({ $binary: { base64: String(base64), subType: hex(Number(subType) & 255, 2) } }, `BinData(${subType}, "${base64}")`);
  const UUID = (text) => {
    let h = text === undefined ? randomHex(16) : String(text).replace(/-/g, "");
    if (text === undefined) {
      // A version 4, variant 1 UUID.
      h = h.slice(0, 12) + "4" + h.slice(13, 16) + ((parseInt(h[16], 16) & 3) | 8).toString(16) + h.slice(17);
    }
    if (!/^[0-9a-fA-F]{32}$/.test(h)) fail("UUID() takes 32 hexadecimal characters");
    const pretty = `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`.toLowerCase();
    return tagged({ $binary: { base64: hexToBase64(h), subType: "04" } }, `UUID("${pretty}")`);
  };
  const MinKey = () => tagged({ $minKey: 1 }, "MinKey()");
  const MaxKey = () => tagged({ $maxKey: 1 }, "MaxKey()");

  // Dates and regular expressions keep their BSON type.
  Object.defineProperty(Date.prototype, "toJSON", {
    configurable: true,
    writable: true,
    value: function () {
      const ms = this.getTime();
      if (isNaN(ms)) fail("Invalid Date");
      return { $date: { $numberLong: String(ms) } };
    },
  });
  Object.defineProperty(RegExp.prototype, "toJSON", {
    configurable: true,
    writable: true,
    value: function () {
      const options = this.flags.split("").filter((f) => "imsux".includes(f)).sort().join("");
      return { $regularExpression: { pattern: this.source, options } };
    },
  });

  // ---- plans ----

  function Plan(target, method, args) {
    this.target = target;
    this.method = method;
    this.args = args;
    this.chain = [];
  }
  const MODIFIERS = [
    "sort", "limit", "skip", "projection", "project", "hint", "maxTimeMS", "collation",
    "batchSize", "comment", "allowDiskUse", "min", "max", "returnKey", "showRecordId",
    "readPref", "readConcern", "noCursorTimeout", "allowPartialResults", "count", "size",
    "itcount", "explain", "toArray", "pretty", "length",
  ];
  for (const m of MODIFIERS) {
    Plan.prototype[m] = function (...args) {
      this.chain.push([m, args]);
      return this;
    };
  }
  for (const m of ["forEach", "map", "next", "hasNext", "tryNext", "close", "objsLeftInBatch", "isExhausted", "then"]) {
    if (m === "then") continue; // a plan is not a promise
    Plan.prototype[m] = function () {
      throw new Error(`.${m}() is not available in a console: the result is shown in the grid`);
    };
  }
  Plan.prototype.toJSON = function () {
    return {
      [PLAN]: {
        db: this.target.db,
        collection: this.target.collection,
        method: this.method,
        args: this.args,
        chain: this.chain,
      },
    };
  };
  Plan.prototype.toString = function () {
    return `${this.target.collection ? `${this.target.db}.${this.target.collection}` : this.target.db}.${this.method}()`;
  };

  const COLLECTION_METHODS = [
    "find", "findOne", "aggregate", "countDocuments", "estimatedDocumentCount", "count",
    "distinct", "insertOne", "insertMany", "insert", "updateOne", "updateMany", "update",
    "replaceOne", "deleteOne", "deleteMany", "remove", "findOneAndUpdate", "findOneAndReplace",
    "findOneAndDelete", "findAndModify", "bulkWrite", "createIndex", "createIndexes", "ensureIndex",
    "dropIndex", "dropIndexes", "getIndexes", "getIndexKeys", "getIndices", "drop",
    "renameCollection", "stats", "dataSize", "storageSize", "totalIndexSize", "totalSize",
    "validate", "isCapped",
  ];

  function Collection(db, name) {
    Object.defineProperty(this, "_db", { value: db });
    Object.defineProperty(this, "_name", { value: name });
  }
  for (const m of COLLECTION_METHODS) {
    Collection.prototype[m] = function (...args) {
      return new Plan({ db: this._db, collection: this._name }, m, args);
    };
  }
  Collection.prototype.getName = function () {
    return this._name;
  };
  Collection.prototype.getFullName = function () {
    return `${this._db}.${this._name}`;
  };
  Collection.prototype.getDB = function () {
    return database(this._db);
  };
  Collection.prototype.toString = Collection.prototype.getFullName;
  Collection.prototype.toJSON = Collection.prototype.getFullName;
  // `db.users.explain("executionStats").find(…)`
  Collection.prototype.explain = function (verbosity = "queryPlanner") {
    const self = this;
    return new Proxy(
      {},
      {
        get(_, method) {
          if (typeof self[method] !== "function") return undefined;
          return (...args) => self[method](...args).explain(verbosity);
        },
      }
    );
  };
  Collection.prototype.watch = function () {
    throw new Error("Change streams are not available in a console");
  };

  function collection(db, name) {
    if (typeof name !== "string" || name === "") fail("A collection name must be a non-empty string");
    return new Proxy(new Collection(db, name), {
      get(target, prop, receiver) {
        if (typeof prop !== "string" || prop in target) return Reflect.get(target, prop, receiver);
        // `db.system.profile`, `db.app.logs`: dotted collection names.
        return collection(db, `${name}.${prop}`);
      },
    });
  }

  const DATABASE_METHODS = [
    "runCommand", "adminCommand", "getCollectionNames", "getCollectionInfos", "createCollection",
    "createView", "dropDatabase", "stats", "serverStatus", "version", "hostInfo", "buildInfo",
    "currentOp", "killOp", "aggregate", "getProfilingStatus", "setProfilingLevel", "getUsers",
    "getRoles", "listCommands", "isMaster", "hello", "serverBuildInfo",
  ];

  function Database(name) {
    Object.defineProperty(this, "_name", { value: name });
  }
  for (const m of DATABASE_METHODS) {
    Database.prototype[m] = function (...args) {
      return new Plan({ db: this._name, collection: null }, m, args);
    };
  }
  Database.prototype.getName = function () {
    return this._name;
  };
  Database.prototype.getCollection = function (name) {
    return collection(this._name, name);
  };
  Database.prototype.getSiblingDB = function (name) {
    return database(name);
  };
  Database.prototype.getMongo = function () {
    return { getDB: (name) => database(name), getDBNames: () => new Database("admin").runCommand({ listDatabases: 1, nameOnly: true }) };
  };
  Database.prototype.toString = Database.prototype.getName;
  Database.prototype.toJSON = Database.prototype.getName;

  function database(name) {
    if (typeof name !== "string" || name === "") fail("A database name must be a non-empty string");
    return new Proxy(new Database(name), {
      get(target, prop, receiver) {
        if (typeof prop !== "string" || prop in target) return Reflect.get(target, prop, receiver);
        return collection(name, prop);
      },
    });
  }

  // ---- printing ----

  let printed = [];
  const show = (v) => {
    if (typeof v === "string") return v;
    if (v === undefined) return "undefined";
    if (v instanceof Date) return v.toISOString();
    try {
      return JSON.stringify(v);
    } catch (e) {
      return String(v);
    }
  };
  const print = (...values) => {
    printed.push(values.map(show).join(" "));
  };
  const printjson = (v) => {
    printed.push(JSON.stringify(v, null, 2));
  };

  Object.assign(global, {
    ObjectId, ISODate, NumberLong, NumberInt, NumberDecimal, Double, Timestamp, BinData, UUID,
    MinKey, MaxKey,
    Long: NumberLong, Int32: NumberInt, Decimal128: NumberDecimal,
    print, printjson, console: { log: print, info: print, warn: print, error: print },
  });

  // Called by Rust: evaluates one statement with `db` bound to `dbName` and
  // returns `{ value, printed }` as JSON; an undefined value is left out.
  Object.defineProperty(global, "__monocodeEval", {
    value: function (source, dbName) {
      printed = [];
      global.db = database(dbName);
      const value = (0, eval)(source);
      const out = { printed };
      if (value !== undefined && typeof value !== "function") out.value = value;
      return JSON.stringify(out);
    },
  });
})(globalThis);
