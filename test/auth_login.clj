(ns test.auth-login
  (:require [clojure.string :as str]
            [clojure.test :refer [deftest is testing]]
            [com.blockether.spel.core :as core]
            [com.blockether.spel.locator :as loc]
            [com.blockether.spel.page :as page]
            [test.support.harness :as harness]
            [test.support.seed-auth :as seed-auth]))

(defn- move-vote-slider-left [pg]
  (page/evaluate pg
                 "(() => { const s = document.getElementById('vote-preference-slider'); if (!s) return; s.value = '20'; s.dispatchEvent(new Event('input', { bubbles: true })); })()"))

(defn- type-alias! [pg text]
  (page/evaluate pg
                 (.replace
                  "(() => { const i = document.getElementById('alias-input'); const f = document.getElementById('alias-check-form'); if (!i || !f) return Promise.resolve('missing-form');
                    i.value = __TEXT__;
                    const cf = document.getElementById('alias-claim-field'); if (cf) cf.value = i.value;
                    return fetch(f.action, { method: 'POST', credentials: 'same-origin',
                      headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
                      body: new URLSearchParams(new FormData(f)).toString() })
                      .then(function (r) { return r.text(); })
                      .then(function (t) { eval(t); return document.getElementById('alias-status')?.textContent || ''; }); })()"
                  "__TEXT__"
                  (pr-str text))))

(defn- element-text [pg selector]
  (let [raw (page/evaluate pg
                           (str "document.querySelector(" (pr-str selector) ")?.textContent || ''"))]
    (when (string? raw) (str/trim raw))))

(defn- wait-for-text [pg test-id text timeout-ms]
  (let [deadline (+ (System/currentTimeMillis) timeout-ms)
        selector (str "[data-testid=\"" test-id "\"]")]
    (loop []
      (let [got (or (element-text pg selector) "")]
        (cond
          (= got text) got
          (< (System/currentTimeMillis) deadline) (do (Thread/sleep 200) (recur))
          :else (throw (ex-info "timeout waiting for text" {:test-id test-id :want text :got got})))))))

(defn- url-query-params [url]
  (let [q (.getRawQuery (java.net.URI. url))]
    (into {}
          (for [pair (when (seq q) (str/split q #"&"))
                :let [[k v] (str/split pair #"=" 2)]
                :when (seq k)]
            [k (java.net.URLDecoder/decode (or v "") "UTF-8")]))))

(defn- query-param [url key]
  (get (url-query-params url) key))

(deftest new-user-login-flow-returns-to-vote-pair
  (testing "anonymous vote redirects through OAuth + alias chooser back to the same pair"
    (let [servers (harness/with-auth-servers
                   (fn [data-dir]
                     (seed-auth/write-seeder-events! (str data-dir "/events.jsonl"))))]
      (try
        (harness/seed-rust-children! (:app-base servers))
        (let [vote-url (seed-auth/seeder-pair-vote-url (:app-base servers))
              alias "newbie-alias"
              expected-params {"parent" seed-auth/rust-scope
                               "left" seed-auth/post-a
                               "right" seed-auth/post-b}]
          (core/with-testing-page [pg]
            (page/navigate pg vote-url)
            (page/wait-for-selector pg "#vote-compare-form")
            (move-vote-slider-left pg)
            (loc/click (page/get-by-test-id pg "vote-post"))
            (page/wait-for-selector pg "[data-testid=oauth-github]" {:timeout 15000})
            (let [login-url (page/url pg)
                  return-to (query-param login-url "return_to")]
              (is (str/includes? login-url "/login?"))
              (is (string? return-to) "login must carry return_to")
              (is (= expected-params (url-query-params (str "http://local" return-to)))
                  "login return_to must point at the shared pair"))
            (loc/click (page/get-by-test-id pg "oauth-github"))
            (page/wait-for-selector pg "[data-testid=alias-input]" {:timeout 15000})
            (type-alias! pg "seeder")
            (wait-for-text pg "alias-status" "taken" 15000)
            (type-alias! pg alias)
            (wait-for-text pg "alias-status" "available" 15000)
            (loc/click (page/get-by-test-id pg "alias-claim"))
            (page/wait-for-selector pg "#vote-compare-form" {:timeout 15000})
            (let [after-url (page/url pg)]
              (is (str/includes? after-url "/vote?"))
              (is (= expected-params (url-query-params after-url))
                  "after login, land on the same shared pair"))
            (loc/click (page/get-by-test-id pg "vote-post"))
            (page/wait-for-selector pg ".vote-edge-history-title" {:timeout 15000})
            (let [history (or (element-text pg "#vote-edge-history-region") "")]
              (is (str/includes? history "votes on this pair"))
              (is (str/includes? history "3:1")
                  "seeded seeder vote still visible")
              (is (not (str/includes? history "no votes on this pair yet"))))))
        (finally
          ((:stop servers)))))))
