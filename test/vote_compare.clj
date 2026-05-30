(ns test.vote-compare
  (:require [babashka.process :as process]
            [clojure.java.io :as io]
            [clojure.string :as str]
            [clojure.test :refer [deftest is testing]]
            [com.blockether.spel.core :as core]
            [com.blockether.spel.locator :as loc]
            [com.blockether.spel.page :as page]
            [test.support.mock-reddit :as mock-reddit])
  (:import [java.net URLEncoder]))

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

(defn- curl-fetch-children [base item]
  (process/shell {:out :string :err :string}
                 "curl" "-sfN" "--max-time" "20"
                 "-X" "POST" (str base "/ui")
                 "--data-urlencode"
                 (str "__rpc__={\"action\":\"fetch_entity\",\"item\":\"" item
                      "\",\"kind\":\"children\"}")))

(defn- vote-page-url [base parent]
  (str base "/vote?parent="
       (URLEncoder/encode parent "UTF-8")))

(deftest vote-compare-shows-recorded-vote-after-post
  (testing "post vote on /vote morphs edge history (mock Reddit children seeded)"
    (let [root (repo-root)
          fixtures (mock-reddit/fixtures-dir root)
          data-dir (.getAbsolutePath
                    (doto (io/file (System/getProperty "java.io.tmpdir")
                                   (str "sorter2-vote-" (System/currentTimeMillis)))
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
            (let [fetch (curl-fetch-children app-base "reddit.com/r/rust")]
              (is (zero? (:exit fetch)) "fetch posts via mock Reddit")
              (is (str/includes? (:out fetch) "Idiomorph.morph")))
            (core/with-testing-page [pg]
              (page/navigate pg (vote-page-url app-base "reddit.com/r/rust"))
              (page/wait-for-selector pg "#vote-compare-form")
              (let [before (loc/text-content (page/locator pg "#vote-edge-history-region"))]
                (is (str/includes? before "no votes on this pair yet")
                    "empty edge history before first vote"))
              (loc/click (page/get-by-test-id pg "vote-post"))
              (page/wait-for-selector pg ".vote-edge-history-title")
              (let [after (loc/text-content (page/locator pg "#vote-edge-history-region"))]
                (is (str/includes? after "votes on this pair")
                    "shows edge history title after vote")
                (is (str/includes? after "1:1")
                    "shows submitted ratio after vote (default slider at center)")
                (is (not (str/includes? after "no votes on this pair yet"))
                    "does not revert to empty edge history")))
            (finally
              (process/destroy proc))))
        (finally
          (stop-mock))))))
