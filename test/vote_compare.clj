(ns test.vote-compare
  (:require [clojure.string :as str]
            [clojure.test :refer [deftest is testing]]
            [com.blockether.spel.core :as core]
            [com.blockether.spel.locator :as loc]
            [com.blockether.spel.page :as page]
            [test.support.harness :as harness]
            [test.support.seed-auth :as seed-auth]))

(defn- element-text [pg selector]
  (let [raw (page/evaluate pg
                           (str "document.querySelector(" (pr-str selector) ")?.textContent || ''"))]
    (when (string? raw) (str/trim raw))))

(defn- wait-for-history-includes [pg needle timeout-ms]
  (let [deadline (+ (System/currentTimeMillis) timeout-ms)]
    (loop []
      (let [got (or (element-text pg "#vote-edge-history-region") "")]
        (cond
          (str/includes? got needle) got
          (< (System/currentTimeMillis) deadline) (do (Thread/sleep 100) (recur))
          :else (throw (ex-info "timeout waiting for edge history update"
                                {:want needle :got got})))))))

(deftest vote-compare-shows-recorded-vote-after-post
  (testing "post vote on /vote morphs edge history (mock Reddit + auth session)"
    (let [servers (harness/with-auth-servers
                   (fn [data-dir]
                     (seed-auth/write-seeder-events! (str data-dir "/events.jsonl"))))]
      (try
        (harness/seed-rust-children! (:app-base servers))
        (let [vote-url (seed-auth/seeder-pair-vote-url (:app-base servers))]
          (core/with-testing-page [pg]
            (page/navigate pg (harness/oauth-login-url (:app-base servers) "/" "1001:seeder"))
            (page/wait-for-selector pg ".top-nav" {:timeout 15000})
            (page/navigate pg vote-url)
            (page/wait-for-selector pg "#vote-compare-form")
            (let [before (or (element-text pg "#vote-edge-history-region") "")]
              (is (str/includes? before "votes on this pair")
                  "seeded seeder vote visible before our vote")
              (is (str/includes? before "3:1")
                  "seeded 3:1 ratio visible before our vote")
              ;; Default slider posts 1:1. Wait for that text — `.vote-edge-history-title`
              ;; already exists before the click, so waiting on it races the async morph.
              (loc/click (page/get-by-test-id pg "vote-post"))
              (let [after (wait-for-history-includes pg "1:1" 15000)]
                (is (str/includes? after "votes on this pair")
                    "shows edge history title after vote")
                (is (not= before after)
                    "edge history updated after authenticated vote")
                (is (not (str/includes? after "no votes on this pair yet"))
                    "does not revert to empty edge history")))))
        (finally
          ((:stop servers)))))))
