(ns test.perf.large-graph
  (:require [clojure.java.io :as io]
            [clojure.test :refer [deftest is testing]]
            [test.perf.server-metrics :as metrics]
            [test.perf.synthetic-graph :as graph]))

(defn- prepare-data-dir!
  [prefix spec]
  (let [dir (metrics/tmp-dir! prefix)
        log-path (str (.getAbsolutePath dir) "/events.jsonl")
        fixture (graph/write-vote-log! log-path spec)]
    {:dir dir
     :fixture fixture}))

(defn- browse-url
  [base scope]
  (str base "/~/" scope))

(defn- assert-under
  [actual limit label]
  (is (and actual (< actual limit))
      (str label " expected < " limit ", got " actual)))

(defn- assert-over
  [actual limit label]
  (is (and actual (> actual limit))
      (str label " expected > " limit ", got " actual)))

(deftest ^:perf cold-start-memory-is-scope-bounded
  (testing "normal startup RSS should be bounded by hot scope size, not total graph size"
    (let [spec (graph/spec-from-env
                "SORTER2_PERF_MEMORY_"
                {:scope-count 10000
                 :item-count 50
                 :votes-per-scope 100})
          max-rss-kb (graph/env-long "SORTER2_PERF_MAX_STARTUP_RSS_KB" (* 80 1024))
          {:keys [dir fixture]} (prepare-data-dir! "sorter2-perf-memory" spec)
          server (metrics/start-server! dir)]
      (try
        (Thread/sleep 500)
        (let [rss (metrics/rss-kb (:pid server))
              result (metrics/report!
                      "cold_start_memory"
                      {:fixture fixture
                       :server (select-keys server [:ready-ms :process-start-ms :pid])
                       :rss-kb rss
                       :target {:max-startup-rss-kb max-rss-kb}})]
          (assert-under (:rss-kb result) max-rss-kb "startup RSS"))
        (finally
          (metrics/stop-server! server))))))

(deftest ^:perf cold-start-time-does-not-scale-with-total-events
  (testing "normal startup should catch up from the projection cursor instead of replaying every event"
    (let [small-spec (graph/spec-from-env
                      "SORTER2_PERF_STARTUP_SMALL_"
                      {:scope-count 1000
                       :item-count 25
                       :votes-per-scope 50})
          large-spec (graph/spec-from-env
                      "SORTER2_PERF_STARTUP_LARGE_"
                      {:scope-count 10000
                       :item-count 25
                       :votes-per-scope 50})
          max-ratio (graph/env-double "SORTER2_PERF_MAX_STARTUP_RATIO" 2.5)
          small (prepare-data-dir! "sorter2-perf-startup-small" small-spec)
          large (prepare-data-dir! "sorter2-perf-startup-large" large-spec)
          small-server (metrics/start-server! (:dir small))
          _ (metrics/stop-server! small-server)
          large-server (metrics/start-server! (:dir large))
          _ (metrics/stop-server! large-server)
          ratio (/ (double (:ready-ms large-server))
                   (max 1.0 (double (:ready-ms small-server))))
          result (metrics/report!
                  "cold_start_scaling"
                  {:small {:fixture (:fixture small)
                           :ready-ms (:ready-ms small-server)}
                   :large {:fixture (:fixture large)
                           :ready-ms (:ready-ms large-server)}
                   :ratio ratio
                   :target {:max-startup-ratio max-ratio}})]
      (assert-under (:ratio result) max-ratio "large/small startup ratio"))))

(deftest ^:perf single-scope-read-is-bounded-and-fast
  (testing "reading one scope should not require resident memory for every other scope"
    (let [spec (graph/spec-from-env
                "SORTER2_PERF_READ_"
                {:scope-count 10000
                 :item-count 50
                 :votes-per-scope 100})
          target-scope (graph/scope-id (quot (:scope-count spec) 2))
          read-count (graph/env-long "SORTER2_PERF_READ_COUNT" 25)
          max-p95-ms (graph/env-double "SORTER2_PERF_MAX_READ_P95_MS" 75.0)
          max-rss-delta-kb (graph/env-long "SORTER2_PERF_MAX_READ_RSS_DELTA_KB" (* 20 1024))
          {:keys [dir fixture]} (prepare-data-dir! "sorter2-perf-read" spec)
          server (metrics/start-server! dir)]
      (try
        (let [rss-before (metrics/rss-kb (:pid server))
              url (browse-url (:base server) target-scope)
              latencies (doall
                         (for [_ (range read-count)]
                           (:ms (metrics/measure-ms
                                 #(metrics/get! url)))))
              rss-after (metrics/rss-kb (:pid server))
              rss-delta (when (and rss-before rss-after) (- rss-after rss-before))
              latency-summary (metrics/summarize-latencies latencies)
              result (metrics/report!
                      "single_scope_read"
                      {:fixture fixture
                       :server (select-keys server [:ready-ms :pid])
                       :scope target-scope
                       :latency latency-summary
                       :rss-before-kb rss-before
                       :rss-after-kb rss-after
                       :rss-delta-kb rss-delta
                       :target {:max-read-p95-ms max-p95-ms
                                :max-read-rss-delta-kb max-rss-delta-kb}})]
          (assert-under (get-in result [:latency :p95-ms]) max-p95-ms "read p95")
          (assert-under (:rss-delta-kb result) max-rss-delta-kb "read RSS delta"))
        (finally
          (metrics/stop-server! server))))))

(deftest ^:perf vote-write-throughput-under-large-existing-graph
  (testing "new votes should stay fast after a huge unrelated graph already exists"
    (let [spec (graph/spec-from-env
                "SORTER2_PERF_WRITE_"
                {:scope-count 10000
                 :item-count 50
                 :votes-per-scope 100})
          write-count (graph/env-long "SORTER2_PERF_WRITE_COUNT" 200)
          min-rps (graph/env-double "SORTER2_PERF_MIN_WRITE_RPS" 200.0)
          max-p95-ms (graph/env-double "SORTER2_PERF_MAX_WRITE_P95_MS" 40.0)
          scope (graph/scope-id 0)
          {:keys [dir fixture]} (prepare-data-dir! "sorter2-perf-write" spec)
          server (metrics/start-server! dir)]
      (try
        (let [started (System/nanoTime)
              latencies (doall
                         (for [i (range write-count)]
                           (let [a (graph/item-id scope (mod i (:item-count spec)))
                                 b (graph/item-id scope (mod (inc i) (:item-count spec)))
                                 rpc (metrics/vote-rpc {:scope scope
                                                        :a a
                                                        :b b
                                                        :ratio-left 2
                                                        :ratio-right 1})]
                             (:ms (metrics/measure-ms
                                   #(metrics/post-ui! (:base server) rpc))))))
              elapsed-sec (/ (- (System/nanoTime) started) 1000000000.0)
              rps (/ write-count elapsed-sec)
              latency-summary (metrics/summarize-latencies latencies)
              result (metrics/report!
                      "vote_write_throughput"
                      {:fixture fixture
                       :server (select-keys server [:ready-ms :pid])
                       :writes write-count
                       :elapsed-sec elapsed-sec
                       :rps rps
                       :latency latency-summary
                       :target {:min-write-rps min-rps
                                :max-write-p95-ms max-p95-ms}})]
          (assert-over (:rps result) min-rps "write throughput")
          (assert-under (get-in result [:latency :p95-ms]) max-p95-ms "write p95"))
        (finally
          (metrics/stop-server! server))))))
