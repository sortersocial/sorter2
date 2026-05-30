(ns test.reddit-import
  (:require [babashka.process :as process]
            [clojure.java.io :as io]
            [clojure.string :as str]
            [clojure.test :refer [deftest is testing]])
  (:import [com.sun.net.httpserver HttpServer HttpHandler HttpExchange]
           [java.net InetSocketAddress]))

(defn- repo-root []
  (.getCanonicalPath (io/file (System/getProperty "user.dir"))))

(defn- pick-port []
  (with-open [s (java.net.ServerSocket. 0)]
    (.getLocalPort s)))

(defn- start-mock-reddit [port fixtures-dir]
  (let [about (.getBytes (slurp (io/file fixtures-dir "r_rust_about.json")) "UTF-8")
        listing (.getBytes (slurp (io/file fixtures-dir "r_rust_listing.json")) "UTF-8")
        server (HttpServer/create (InetSocketAddress. "127.0.0.1" port) 0)
        handler
        (proxy [HttpHandler] []
          (handle [^HttpExchange exchange]
            ;; Route by path: `/r/<sub>/about.json` is the subreddit entity,
            ;; `/r/<sub>.json` is the children listing.
            (let [path (.getPath (.getRequestURI exchange))
                  body (if (str/includes? path "/about")
                         about
                         listing)]
              (.sendResponseHeaders exchange 200 (alength body))
              (let [out (.getResponseBody exchange)]
                (.write out body)
                (.close out)))))]
    (.createContext server "/" handler)
    (.setExecutor server nil)
    (.start server)
    (fn stop []
      (.stop server 0))))

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

(deftest reddit-fetch-via-mock-api
  (testing "Fetch more queues import; event log stores full payload; page shows title"
    (let [root (repo-root)
          fixtures (str root "/test/fixtures/reddit")
          data-dir (.getAbsolutePath
                    (doto (io/file (System/getProperty "java.io.tmpdir")
                                   (str "sorter2-reddit-" (System/currentTimeMillis)))
                      (.mkdirs)))
          reddit-port (pick-port)
          app-port (pick-port)
          reddit-base (str "http://127.0.0.1:" reddit-port)
          app-base (str "http://127.0.0.1:" app-port)
          bin (str root "/target/release/sorter2-server")
          stop-mock (start-mock-reddit reddit-port fixtures)]
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
            (let [browse-url (str app-base "/~/https://reddit.com/r/rust")
                  before (:out (process/shell {:out :string :err :string}
                                              "curl" "-sf" browse-url))]
              (is (str/includes? before "Fetch from Reddit"))
              (is (not (str/includes? before "The Rust Programming Language")))
              (let [log-path (str data-dir "/events.jsonl")
                    sse (curl-fetch-ui-sse app-base "reddit.com/r/rust")]
                (is (zero? (:exit sse)) "POST /ui fetch_entity SSE succeeds")
                (is (str/includes? (:out sse) "event: complete"))
                (is (str/includes? (:out sse) "The Rust Programming Language"))
                (is (wait-event-log log-path 2000) "event log written")
                (let [after (:out (process/shell {:out :string :err :string}
                                                 "curl" "-sf" browse-url))
                      log (slurp (io/file log-path))]
                  (is (str/includes? after "The Rust Programming Language"))
                  (is (str/includes? log "\"type\":\"entity_imported\""))
                  (is (str/includes? log "\"subscribers\":350000"))
                  (is (str/includes? log "\"display_name\":\"rust\"")))))
            (finally
              (process/destroy proc))))
        (finally
          (stop-mock))))))
