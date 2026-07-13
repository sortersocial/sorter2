(ns test.support.harness
  (:require [babashka.process :as process]
            [clojure.java.io :as io]
            [clojure.string :as str]
            [test.support.mock-oauth :as mock-oauth]
            [test.support.mock-reddit :as mock-reddit]))

(defn repo-root []
  (.getCanonicalPath (io/file (System/getProperty "user.dir"))))

(defn pick-port []
  (with-open [s (java.net.ServerSocket. 0)]
    (.getLocalPort s)))

(defn wait-health [base-url ms]
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

(defn curl-fetch-children [base item]
  (process/shell {:out :string :err :string}
                 "curl" "-sfN" "--max-time" "20"
                 "-X" "POST" (str base "/ui")
                 "--data-urlencode"
                 (str "__rpc__={\"action\":\"fetch_entity\",\"item\":\"" item
                      "\",\"kind\":\"children\"}")))

(defn app-env
  [data-dir app-port oauth-port reddit-port]
  (into (into {} (System/getenv))
        {"SORTER2_SKIP_DOTENV" "1"
         "SORTER2_DATA_DIR" data-dir
         "SORTER2_EVENT_LOG" (str data-dir "/events.jsonl")
         "SORTER2_VIEWS_LOG" (str data-dir "/views.jsonl")
         "PORT" (str app-port)
         "SORTER2_BASE_URL" (str "http://127.0.0.1:" app-port)
         "SORTER2_ALLOW_MOCK_OAUTH" "1"
         "GITHUB_CLIENT_ID" "test-client"
         "GITHUB_CLIENT_SECRET" "test-secret"
         "GITHUB_OAUTH_BASE" (str "http://127.0.0.1:" oauth-port)
         "GITHUB_API_BASE" (str "http://127.0.0.1:" oauth-port)
         "REDDIT_API_BASE" (str "http://127.0.0.1:" reddit-port)
         "REDDIT_OAUTH_BASE" (str "http://127.0.0.1:" reddit-port)
         "REDDIT_CLIENT_ID" ""
         "REDDIT_CLIENT_SECRET" ""
         "REDDIT_APP_ID" ""
         "REDDIT_APP_SECRET" ""}))

(defn with-auth-servers
  "Start mock Reddit + mock OAuth + release sorter2-server.
   `seed-fn` is `(fn [data-dir] ...)` called before the app boots.
   Returns `{:stop ... :app-base ...}`."
  [seed-fn]
  (let [root (repo-root)
        fixtures (mock-reddit/fixtures-dir root)
        data-dir (.getAbsolutePath
                  (doto (io/file (System/getProperty "java.io.tmpdir")
                                 (str "sorter2-auth-" (System/currentTimeMillis)))
                    (.mkdirs)))
        reddit-port (pick-port)
        oauth-port (pick-port)
        app-port (pick-port)
        app-base (str "http://127.0.0.1:" app-port)
        bin (str root "/target/release/sorter2-server")
        stop-mock-reddit (mock-reddit/start-mock-reddit reddit-port fixtures)
        stop-mock-oauth (mock-oauth/start-mock-oauth oauth-port)]
    (seed-fn data-dir)
    (process/shell {:dir root}
                   "cargo" "build" "--release" "--package" "sorter2-server")
    (let [proc (process/process {:dir root
                                 :env (app-env data-dir app-port oauth-port reddit-port)
                                 :out :string
                                 :err :string}
                                bin)]
      (when-not (wait-health app-base 25000)
        (process/destroy proc)
        (stop-mock-oauth)
        (stop-mock-reddit)
        (throw (ex-info "app healthz timeout" {:app-base app-base})))
      {:stop (fn []
               (process/destroy proc)
               (stop-mock-oauth)
               (stop-mock-reddit))
       :app-base app-base
       :data-dir data-dir})))

(defn oauth-login-url
  [app-base return-to mock-user]
  (str app-base "/auth/github?return_to="
       (java.net.URLEncoder/encode return-to "UTF-8")
       "&mock_user=" (java.net.URLEncoder/encode mock-user "UTF-8")))

(defn seed-rust-children! [app-base]
  (let [fetch (curl-fetch-children app-base "reddit.com/r/rust")]
    (when-not (zero? (:exit fetch))
      (throw (ex-info "fetch rust children failed" {:err (:err fetch)})))))
