(ns test.perf.server-metrics
  (:require [babashka.process :as process]
            [clojure.java.io :as io]
            [clojure.pprint :as pprint]
            [clojure.string :as str]
            [test.perf.synthetic-graph :as graph])
  (:import [java.net URL URLEncoder HttpURLConnection]))

(defn repo-root []
  (.getCanonicalPath (io/file (System/getProperty "user.dir"))))

(defn pick-port []
  (with-open [s (java.net.ServerSocket. 0)]
    (.getLocalPort s)))

(defonce release-built? (atom false))

(defn tmp-dir!
  [prefix]
  (doto (io/file (System/getProperty "java.io.tmpdir")
                 (str prefix "-" (System/currentTimeMillis) "-" (rand-int 1000000)))
    (.mkdirs)))

(defn ensure-release-binary!
  []
  (let [root (repo-root)
        bin (io/file root "target/release/sorter2-server")]
    (when-not @release-built?
      (let [result (process/shell {:dir root}
                                  "cargo" "build" "--release" "--package" "sorter2-server")]
        (when-not (zero? (:exit result))
          (throw (ex-info "release build failed" result)))
        (reset! release-built? true)))
    (.getAbsolutePath bin)))

(defn- read-stream
  [stream]
  (when stream
    (slurp stream)))

(defn http-request
  [{:keys [method url body content-type timeout-ms]
    :or {method "GET" timeout-ms 30000}}]
  (let [conn ^HttpURLConnection (.openConnection (URL. url))]
    (.setRequestMethod conn method)
    (.setConnectTimeout conn timeout-ms)
    (.setReadTimeout conn timeout-ms)
    (when body
      (.setDoOutput conn true)
      (.setRequestProperty conn "Content-Type" content-type)
      (with-open [out (.getOutputStream conn)]
        (.write out (.getBytes body "UTF-8"))))
    (let [code (.getResponseCode conn)
          stream (if (< code 400) (.getInputStream conn) (.getErrorStream conn))
          response (or (read-stream stream) "")]
      (when (>= code 400)
        (throw (ex-info "HTTP request failed" {:url url :code code :body response})))
      {:status code :body response})))

(defn form-encode
  [m]
  (str/join "&"
            (for [[k v] m]
              (str (URLEncoder/encode (str k) "UTF-8")
                   "="
                   (URLEncoder/encode (str v) "UTF-8")))))

(defn post-ui!
  [base rpc]
  (http-request {:method "POST"
                 :url (str base "/ui")
                 :content-type "application/x-www-form-urlencoded"
                 :body (form-encode {"__rpc__" rpc})}))

(defn get!
  [url]
  (http-request {:url url}))

(defn wait-health
  [base-url timeout-ms]
  (let [deadline (+ (System/currentTimeMillis) timeout-ms)
        started (System/nanoTime)
        health-url (str base-url "/healthz")]
    (loop []
      (let [ok? (try
                  (= "ok" (str/trim (:body (get! health-url))))
                  (catch Exception _ false))]
        (cond
          ok? (long (/ (- (System/nanoTime) started) 1000000))
          (< (System/currentTimeMillis) deadline) (do (Thread/sleep 100) (recur))
          :else (throw (ex-info "server did not become healthy"
                                {:base-url base-url :timeout-ms timeout-ms})))))))

(defn process-pid
  [proc]
  (cond
    (instance? java.lang.Process proc) (.pid ^java.lang.Process proc)
    (:pid proc) (:pid proc)
    (:proc proc) (.pid ^java.lang.Process (:proc proc))
    :else (throw (ex-info "cannot determine process pid" {:process proc}))))

(defn start-server!
  [data-dir]
  (let [bin (ensure-release-binary!)
        port (pick-port)
        base (str "http://127.0.0.1:" port)
        event-log (str (.getAbsolutePath (io/file data-dir)) "/events.jsonl")
        started (System/nanoTime)
        proc (process/process {:dir (repo-root)
                               :env (into (into {} (System/getenv))
                                          {"SORTER2_SKIP_DOTENV" "1"
                                           "SORTER2_DATA_DIR" (.getAbsolutePath (io/file data-dir))
                                           "SORTER2_EVENT_LOG" event-log
                                           "PORT" (str port)})
                               :out :string
                               :err :string}
                              bin)
        ready-ms (wait-health base (graph/env-long "SORTER2_PERF_HEALTH_TIMEOUT_MS" 180000))]
    {:proc proc
     :pid (process-pid proc)
     :port port
     :base base
     :event-log event-log
     :ready-ms ready-ms
     :process-start-ms (long (/ (- (System/nanoTime) started) 1000000))}))

(defn stop-server!
  [server]
  (when-let [proc (:proc server)]
    (process/destroy proc)))

(defn rss-kb
  [pid]
  (let [status-file (io/file "/proc" (str pid) "status")]
    (when (.exists status-file)
      (with-open [reader (java.io.BufferedReader. (java.io.FileReader. status-file))]
        (loop []
          (when-let [line (.readLine reader)]
            (if-let [[_ kb] (re-matches #"VmRSS:\s+(\d+)\s+kB" line)]
              (Long/parseLong kb)
              (recur))))))))

(defn measure-ms
  [f]
  (let [started (System/nanoTime)
        value (f)]
    {:value value
     :ms (double (/ (- (System/nanoTime) started) 1000000.0))}))

(defn percentile
  [xs pct]
  (let [v (vec (sort xs))
        n (count v)]
    (when (pos? n)
      (nth v (min (dec n) (long (Math/floor (* pct (dec n)))))))))

(defn summarize-latencies
  [latencies-ms]
  {:count (count latencies-ms)
   :min-ms (first (sort latencies-ms))
   :p50-ms (percentile latencies-ms 0.50)
   :p95-ms (percentile latencies-ms 0.95)
   :max-ms (last (sort latencies-ms))})

(defn report!
  [name result]
  (let [dir (doto (io/file (repo-root) "target/perf") (.mkdirs))
        file (io/file dir (str name ".edn"))]
    (spit file (with-out-str (pprint/pprint result)))
    (println "PERF" name (pr-str (assoc result :report-file (.getAbsolutePath file))))
    result))

(defn vote-rpc
  [{:keys [scope a b ratio-left ratio-right]}]
  (str "{\"action\":\"record_vote\""
       ",\"a\":" (graph/json-string a)
       ",\"b\":" (graph/json-string b)
       ",\"ratio_left\":" ratio-left
       ",\"ratio_right\":" ratio-right
       ",\"scope\":" (graph/json-string scope)
       "}"))
