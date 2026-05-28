(ns test.smoke
  (:require [babashka.process :as process]
            [clojure.java.io :as io]
            [clojure.string :as str]
            [clojure.test :refer [deftest is testing]]))

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

(deftest http-smoke-against-running-server
  (testing "build, start, healthz, home contains main panels"
    (let [root (repo-root)
          data-dir (.getAbsolutePath (doto (io/file (System/getProperty "java.io.tmpdir")
                                                   (str "sorter2-smoke-" (System/currentTimeMillis)))
                                     (.mkdirs)))
          port (pick-port)
          base (str "http://127.0.0.1:" port)
          bin (str root "/target/release/sorter2-server")]
      (is (zero? (:exit (process/shell {:dir root}
                                      "cargo" "build" "--release" "--package" "sorter2-server")))
          "release build succeeds")
      (is (.exists (io/file bin)) "binary exists")
      (let [proc (process/process {:dir root
                                   :env {"SORTER2_DATA_DIR" data-dir
                                         "SORTER2_EVENT_LOG" (str data-dir "/events.jsonl")
                                         "PORT" (str port)}
                                   :out :string
                                   :err :string}
                                  bin)]
        (try
          (is (wait-health base 15000) "server responds to /healthz")
          (let [home (:out (process/shell {:out :string :err :string}
                                         "curl" "-sf" (str base "/")))]
            (is (str/includes? home "vote-panel"))
            (is (str/includes? home "ranking-panel"))
            (is (str/includes? home "parser-panel"))
            (is (str/includes? home "__rpc__")))
          (finally
            (process/destroy proc)))))))
