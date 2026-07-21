(ns test.reddit-import
  (:require [babashka.process :as process]
            [clojure.java.io :as io]
            [clojure.string :as str]
            [clojure.test :refer [deftest is testing]]
            [test.support.mock-reddit :as mock-reddit]))

(defn- repo-root []
  (.getCanonicalPath (io/file (System/getProperty "user.dir"))))

(defn- pick-port []
  (with-open [s (java.net.ServerSocket. 0)]
    (.getLocalPort s)))

(defn- wait-health [base-url ms]
  (let [deadline (+ (System/currentTimeMillis) ms)
        url (str base-url "/healthz")]
    (loop []
      (let [resp (try
                   (process/shell {:out :string :err :string}
                                  "curl" "-sf" url)
                   (catch Exception _ nil))]
        (if (and resp (zero? (:exit resp)) (= "ok" (str/trim (:out resp ""))))
          true
          (if (< (System/currentTimeMillis) deadline)
            (do (Thread/sleep 200) (recur))
            false))))))

(defn- curl-fetch-ui-sse [base item kind]
  (process/shell {:out :string :err :string}
                 "curl" "-sfN" "--max-time" "20"
                 "-X" "POST" (str base "/ui")
                 "--data-urlencode"
                 (str "__rpc__={\"action\":\"fetch_entity\",\"item\":\"" item
                      "\",\"kind\":\"" kind "\"}")))

(defn- wait-event-log [path ms]
  (let [deadline (+ (System/currentTimeMillis) ms)]
    (loop []
      (if (.exists (io/file path))
        true
        (if (< (System/currentTimeMillis) deadline)
          (do (Thread/sleep 200) (recur))
          false)))))

(defn- run-reddit-fetch-assertions [app-base data-dir]
  (let [browse-url (str app-base "/~/https://reddit.com/r/rust")
        log-path (str data-dir "/events.jsonl")
        before (:out (process/shell {:out :string :err :string}
                                    "curl" "-sf" browse-url))]
    (is (str/includes? before "Fetch from Reddit"))
    (is (not (str/includes? before "The Rust Programming Language")))
    (let [sse (curl-fetch-ui-sse app-base "reddit.com/r/rust" "self")]
      (is (zero? (:exit sse)) "POST /ui fetch_entity (self) SSE succeeds")
      (is (str/includes? (:out sse) "Idiomorph.morph"))
      (is (str/includes? (:out sse) "The Rust Programming Language"))
      (is (wait-event-log log-path 2000) "event log written"))
    (let [after (:out (process/shell {:out :string :err :string}
                                     "curl" "-sf" browse-url))
          log (slurp (io/file log-path))]
      (is (str/includes? after "The Rust Programming Language"))
      (is (str/includes? log "\"type\":\"node_ensured\""))
      (is (not (str/includes? log "\"subscribers\"")))
      (is (not (str/includes? log "\"display_name\"")))
      (is (not (str/includes? log "entity_imported"))))
    (let [children-sse (curl-fetch-ui-sse app-base "reddit.com/r/rust" "children")]
      (is (zero? (:exit children-sse)) "POST /ui fetch_entity (children) SSE succeeds")
      (is (str/includes? (:out children-sse) "Idiomorph.morph"))
      (is (str/includes? (:out children-sse) "Announcing Rust 1.99")))
    (let [after-children (:out (process/shell {:out :string :err :string}
                                              "curl" "-sf" browse-url))
          log2 (slurp (io/file log-path))]
      (is (str/includes? after-children "Announcing Rust 1.99"))
      (is (str/includes? after-children "Unranked"))
      (is (str/includes? after-children "Refresh ranking"))
      (is (str/includes? log2 "\"type\":\"node_ensured\""))
      (is (str/includes? log2 "/comments/"))
      (is (not (str/includes? log2 "\"selftext\""))))
    (let [log-before-ranked (slurp (io/file log-path))
          ranked-sse (curl-fetch-ui-sse app-base "reddit.com/r/rust" "ranked")]
      (is (zero? (:exit ranked-sse)) "POST /ui fetch_entity (ranked) SSE succeeds")
      (is (str/includes? (:out ranked-sse) "Idiomorph.morph"))
      (is (str/includes? (:out ranked-sse) "Announcing Rust 1.99"))
      (is (str/includes? (:out ranked-sse) "What are you working on this week?"))
      ;; Ranked refresh persists refreshed safety classifications, but must not
      ;; append duplicate structure events.
      (let [log-after-ranked (slurp (io/file log-path))]
        (is (= (count (re-seq #"\"type\":\"node_ensured\"" log-before-ranked))
               (count (re-seq #"\"type\":\"node_ensured\"" log-after-ranked))))
        (is (> (count (re-seq #"\"type\":\"nsfw_classified\"" log-after-ranked))
               (count (re-seq #"\"type\":\"nsfw_classified\"" log-before-ranked))))))))

(deftest reddit-fetch-via-mock-api
  (testing "Fetch caches display content ephemerally; log records structure and safety"
    (let [root (repo-root)
          fixtures (mock-reddit/fixtures-dir root)
          data-dir (.getAbsolutePath
                    (doto (io/file (System/getProperty "java.io.tmpdir")
                                   (str "sorter2-reddit-" (System/currentTimeMillis)))
                      (.mkdirs)))
          reddit-port (pick-port)
          app-port (pick-port)
          reddit-base (str "http://127.0.0.1:" reddit-port)
          app-base (str "http://127.0.0.1:" app-port)
          bin (str root "/target/release/sorter2-server")
          stop-mock (mock-reddit/start-mock-reddit reddit-port fixtures)]
      (try
        (is (zero? (:exit (process/shell {:dir root}
                                         "cargo" "build" "--release" "--package" "sorter2-server")))
            "release build succeeds")
        (let [proc (process/process {:dir root
                                     :env (into (into {} (System/getenv))
                                                {"SORTER2_SKIP_DOTENV" "1"
                                                 "SORTER2_DATA_DIR" data-dir
                                                 "SORTER2_EVENT_LOG" (str data-dir "/events.jsonl")
                                                 "PORT" (str app-port)
                                                 "REDDIT_API_BASE" reddit-base
                                                 "REDDIT_OAUTH_BASE" reddit-base
                                                 "REDDIT_CLIENT_ID" ""
                                                 "REDDIT_CLIENT_SECRET" ""
                                                 "REDDIT_APP_ID" ""
                                                 "REDDIT_APP_SECRET" ""})
                                     :out :string
                                     :err :string}
                                    bin)]
          (try
            (is (wait-health app-base 20000) "app healthz")
            (run-reddit-fetch-assertions app-base data-dir)
            (finally
              (process/destroy proc))))
        (finally
          (stop-mock))))))
