(ns test.parser-race
  "Browser test (spel / Playwright) for the parser search-box response race.

   The search box debounces keystrokes, POSTs each to `/ui`, and eval()s the
   returned JS, which morphs `#parser-panel` (input value + `#parser-output`).
   If responses are applied in arrival order with no ordering guard, a slow
   response for an *earlier* query can land after a newer one and clobber it.

   To force the race deterministically without touching the Rust server, the
   browser talks to a small in-process reverse proxy that injects an asymmetric
   per-query delay: the earlier query (`r/rust`) is delayed far longer than the
   later query (`r/aww`). The later query therefore renders first, then the
   stale earlier response arrives. Correct behavior: the panel reflects the
   *latest* query the user typed (`r/aww`)."
  (:require [babashka.process :as process]
            [clojure.java.io :as io]
            [clojure.string :as str]
            [clojure.test :refer [deftest is testing]]
            [com.blockether.spel.assertions :as assert]
            [com.blockether.spel.core :as core]
            [com.blockether.spel.locator :as locator]
            [com.blockether.spel.page :as page])
  (:import [com.sun.net.httpserver HttpServer HttpHandler]
           [java.io ByteArrayOutputStream]
           [java.net InetSocketAddress URI URLDecoder]
           [java.net.http HttpClient HttpClient$Version HttpRequest
            HttpRequest$BodyPublishers HttpResponse$BodyHandlers]
           [java.nio.charset StandardCharsets]
           [java.util.concurrent Executors]))

(def ^:private slow-query "r/rust")
(def ^:private fast-query "r/aww")
(def ^:private slow-delay-ms 800)

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
                   (process/shell {:out :string :err :string} "curl" "-sf" url)
                   (catch Exception _ nil))]
        (if (and resp (zero? (:exit resp)) (= "ok" (str/trim (:out resp ""))))
          true
          (if (< (System/currentTimeMillis) deadline)
            (do (Thread/sleep 200) (recur))
            false))))))

(defn- start-server
  "Builds the release binary and starts it on a random port. Returns a map with
   :proc and :base."
  [root]
  (is (zero? (:exit (process/shell {:dir root}
                                   "cargo" "build" "--release" "--package" "sorter2-server")))
      "release build succeeds")
  (let [bin (str root "/target/release/sorter2-server")]
    (is (.exists (io/file bin)) "binary exists")
    (let [data-dir (.getAbsolutePath
                    (doto (io/file (System/getProperty "java.io.tmpdir")
                                   (str "sorter2-race-" (System/currentTimeMillis)))
                      (.mkdirs)))
          port (pick-port)
          base (str "http://127.0.0.1:" port)
          proc (process/process {:dir root
                                 :env {"SORTER2_DATA_DIR" data-dir
                                       "SORTER2_EVENT_LOG" (str data-dir "/events.jsonl")
                                       "PORT" (str port)}
                                 :out :string
                                 :err :string}
                                bin)]
      {:proc proc :base base})))

(defn- read-all-bytes ^bytes [in]
  (let [bos (ByteArrayOutputStream.)]
    (io/copy in bos)
    (.toByteArray bos)))

(defn- ui-query
  "Extracts the decoded `query` form field from a urlencoded body, or nil."
  [body-str]
  (some-> (re-find #"(?:^|&)query=([^&]*)" body-str)
          second
          (URLDecoder/decode "UTF-8")))

(defn- start-proxy
  "Reverse proxy to `upstream` that sleeps `(delay-fn method path body-str)` ms
   before forwarding each request. Runs handlers on a thread pool so concurrent
   requests are delayed independently (the race needs out-of-order arrival).
   Returns a map with :server and :base."
  [upstream delay-fn]
  (let [client (-> (HttpClient/newBuilder)
                   (.version HttpClient$Version/HTTP_1_1)
                   (.build))
        server (HttpServer/create (InetSocketAddress. "127.0.0.1" 0) 0)
        handler (reify HttpHandler
                  (handle [_ ex]
                    (try
                      (let [method (.getRequestMethod ex)
                            uri (.getRequestURI ex)
                            path (.getRawPath uri)
                            query (.getRawQuery uri)
                            req-body (read-all-bytes (.getRequestBody ex))
                            body-str (String. req-body StandardCharsets/UTF_8)
                            delay-ms (delay-fn method path body-str)]
                        (when (pos? delay-ms)
                          (Thread/sleep (long delay-ms)))
                        (let [target (str upstream path (when query (str "?" query)))
                              builder (doto (HttpRequest/newBuilder)
                                        (.uri (URI/create target)))
                              ct (.getFirst (.getRequestHeaders ex) "Content-Type")
                              _ (when ct (.header builder "Content-Type" ct))
                              publisher (if (zero? (alength req-body))
                                          (HttpRequest$BodyPublishers/noBody)
                                          (HttpRequest$BodyPublishers/ofByteArray req-body))
                              _ (.method builder method publisher)
                              resp (.send client (.build builder)
                                          (HttpResponse$BodyHandlers/ofByteArray))
                              resp-body (.body resp)
                              resp-ct (-> (.headers resp)
                                          (.firstValue "content-type")
                                          (.orElse nil))]
                          (when resp-ct
                            (.set (.getResponseHeaders ex) "Content-Type" resp-ct))
                          (.sendResponseHeaders ex (.statusCode resp) (alength resp-body))
                          (with-open [os (.getResponseBody ex)]
                            (.write os resp-body))))
                      (catch Throwable t
                        (let [msg (.getBytes (str "proxy error: " (.getMessage t))
                                             StandardCharsets/UTF_8)]
                          (try
                            (.sendResponseHeaders ex 500 (alength msg))
                            (with-open [os (.getResponseBody ex)]
                              (.write os msg))
                            (catch Throwable _ nil))))
                      (finally
                        (.close ex)))))]
    (.createContext server "/" handler)
    (.setExecutor server (Executors/newCachedThreadPool))
    (.start server)
    {:server server
     :base (str "http://127.0.0.1:" (.getPort (.getAddress server)))}))

(defn- parser-delay-fn [method path body-str]
  (if (and (= method "POST") (= path "/ui") (= (ui-query body-str) slow-query))
    slow-delay-ms
    0))

(deftest latest-search-query-wins
  (testing "a slow earlier parser response must not clobber a newer one"
    (let [root (repo-root)
          {:keys [proc base]} (start-server root)]
      (try
        (is (wait-health base 15000) "server responds to /healthz")
        (let [{:keys [server] proxy-base :base} (start-proxy base parser-delay-fn)]
          (try
            (core/with-testing-page [pg]
              (page/navigate pg proxy-base)
              (let [input (page/locator pg "#parser-input")
                    output (page/locator pg "#parser-output")]
                ;; Type the slow query first; wait past the 120ms debounce so its
                ;; (slow) request is in flight, then type the fast query.
                (locator/fill input slow-query)
                (Thread/sleep 300)
                (locator/fill input fast-query)
                ;; Wait until the slow response has certainly arrived and been
                ;; (mis)applied if the race exists.
                (Thread/sleep 2000)
                ;; The panel must reflect the latest query, not the stale one.
                (is (nil? (assert/contains-text (assert/assert-that output) fast-query))
                    "output shows the latest query (r/aww)")
                (is (nil? (assert/contains-text
                           (assert/loc-not (assert/assert-that output)) slow-query))
                    "output does NOT show the stale earlier query (r/rust)")
                (is (nil? (assert/has-value (assert/assert-that input) fast-query))
                    "input value is the latest query (r/aww)")))
            (finally
              (.stop server 0))))
        (finally
          (process/destroy proc))))))
