(ns test.perf.synthetic-graph
  (:require [clojure.java.io :as io]
            [clojure.string :as str]))

(defn env-long
  [name default]
  (if-let [raw (System/getenv name)]
    (Long/parseLong raw)
    default))

(defn env-double
  [name default]
  (if-let [raw (System/getenv name)]
    (Double/parseDouble raw)
    default))

(defn json-string
  [s]
  (str "\""
       (-> (str s)
           (str/replace "\\" "\\\\")
           (str/replace "\"" "\\\"")
           (str/replace "\n" "\\n")
           (str/replace "\r" "\\r")
           (str/replace "\t" "\\t"))
       "\""))

(defn scope-id
  [scope-idx]
  (format "reddit.com/r/perf-s%06d" scope-idx))

(defn item-id
  [scope item-idx]
  (format "%s/post-%05d" scope item-idx))

(defn vote-line
  [{:keys [ts scope a b ratio-left ratio-right]}]
  (str "{\"type\":\"vote_recorded\""
       ",\"ts\":" ts
       ",\"a\":" (json-string a)
       ",\"b\":" (json-string b)
       ",\"ratio_left\":" ratio-left
       ",\"ratio_right\":" ratio-right
       ",\"scope\":" (json-string scope)
       "}\n"))

(defn vote-for
  [scope-idx item-count vote-idx]
  (let [scope (scope-id scope-idx)
        a-idx (mod vote-idx item-count)
        b-idx (mod (inc vote-idx) item-count)]
    {:ts (+ 1700000000000 (* scope-idx 1000000) vote-idx)
     :scope scope
     :a (item-id scope a-idx)
     :b (item-id scope b-idx)
     :ratio-left (inc (mod vote-idx 5))
     :ratio-right (inc (mod (+ vote-idx 2) 5))}))

(defn write-vote-log!
  "Write a synthetic multi-scope vote graph as JSONL and return fixture metadata."
  [path {:keys [scope-count item-count votes-per-scope]}]
  (let [file (io/file path)
        started (System/nanoTime)]
    (.mkdirs (.getParentFile file))
    (with-open [w (java.io.BufferedWriter. (io/writer file))]
      (doseq [scope-idx (range scope-count)
              vote-idx (range votes-per-scope)]
        (.write w (vote-line (vote-for scope-idx item-count vote-idx)))))
    {:path (.getAbsolutePath file)
     :scope-count scope-count
     :item-count item-count
     :votes-per-scope votes-per-scope
     :event-count (* scope-count votes-per-scope)
     :bytes (.length file)
     :write-ms (long (/ (- (System/nanoTime) started) 1000000))}))

(defn spec-from-env
  [prefix defaults]
  {:scope-count (env-long (str prefix "SCOPE_COUNT") (:scope-count defaults))
   :item-count (env-long (str prefix "ITEM_COUNT") (:item-count defaults))
   :votes-per-scope (env-long (str prefix "VOTES_PER_SCOPE") (:votes-per-scope defaults))})
