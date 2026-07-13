(ns test.support.mock-reddit
  "In-process HTTP stub for Reddit API fixtures + OAuth login endpoints."
  (:require [clojure.java.io :as io]
            [clojure.string :as str])
  (:import [com.sun.net.httpserver HttpServer HttpHandler HttpExchange]
           [java.net InetSocketAddress URLDecoder]))

(defn fixtures-dir
  ([] (fixtures-dir (System/getProperty "user.dir")))
  ([root] (str root "/test/fixtures/reddit")))

(defn- query-param [query key]
  (when query
    (some (fn [pair]
            (let [[k v] (str/split pair "=" 2)]
              (when (= k key)
                (URLDecoder/decode (or v "") "UTF-8"))))
          (str/split query #"&"))))

(defn- parse-mock-user [raw]
  (let [s (or raw "t2_test:redditor")
        [id login] (str/split s #":" 2)]
    {:id id :login (or login "redditor")}))

(defn- send-bytes [^HttpExchange ex status ^bytes body content-type]
  (.set (.getResponseHeaders ex) "Content-Type" content-type)
  (.sendResponseHeaders ex status (alength body))
  (doto (.getResponseBody ex)
    (.write body)
    (.close)))

(defn- send-json [^HttpExchange ex status body]
  (send-bytes ex status (.getBytes body "UTF-8") "application/json"))

(defn- send-redirect [^HttpExchange ex location]
  (.set (.getResponseHeaders ex) "Location" location)
  (.sendResponseHeaders ex 302 -1)
  (.close (.getResponseBody ex)))

(defn- read-form [^HttpExchange ex]
  (let [body (slurp (.getInputStream ex))]
    {:code (query-param body "code")
     :grant (query-param body "grant_type")}))

(defn- bearer-token [^HttpExchange ex]
  (some-> (.getRequestHeaders ex)
          (.getFirst "Authorization")
          (str/replace #"^[Bb]earer " "")))

(defn- parse-token-user [token]
  (when (str/starts-with? token "mock:")
    (parse-mock-user (subs token 5))))

(defn start-mock-reddit
  "Start a mock Reddit API on `port`. Returns a zero-arg `stop` function."
  ([port] (start-mock-reddit port (fixtures-dir)))
  ([port dir]
   (let [about (.getBytes (slurp (io/file dir "r_rust_about.json")) "UTF-8")
         listing (.getBytes (slurp (io/file dir "r_rust_listing.json")) "UTF-8")
         server (HttpServer/create (InetSocketAddress. "127.0.0.1" port) 0)
         handler
         (proxy [HttpHandler] []
           (handle [^HttpExchange exchange]
             (let [uri (.getRequestURI exchange)
                   path (.getPath uri)
                   query (.getQuery uri)
                   method (.getRequestMethod exchange)]
               (cond
                 (str/ends-with? path "/api/v1/authorize")
                 (let [redirect-uri (query-param query "redirect_uri")
                       state (query-param query "state")
                       user (parse-mock-user (query-param query "mock_user"))
                       code (str "mock:" (:id user) ":" (:login user))
                       loc (str redirect-uri "?code=" (java.net.URLEncoder/encode code "UTF-8")
                                "&state=" (java.net.URLEncoder/encode state "UTF-8"))]
                   (send-redirect exchange loc))

                 (and (= method "POST") (str/ends-with? path "/api/v1/access_token"))
                 (let [form (read-form exchange)
                       grant (or (:grant form) "")
                       code (or (:code form) "mock:t2_test:redditor")]
                   (if (= grant "client_credentials")
                     (send-json exchange 200 "{\"access_token\":\"app-token\",\"token_type\":\"bearer\",\"expires_in\":3600}")
                     (send-json exchange 200 (str "{\"access_token\":\"" code "\",\"token_type\":\"bearer\",\"expires_in\":3600}"))))

                 (str/ends-with? path "/api/v1/me")
                 (let [user (or (parse-token-user (bearer-token exchange))
                                {:id "t2_test" :login "redditor"})]
                   (send-json exchange 200
                              (str "{\"id\":\"" (:id user) "\",\"name\":\"" (:login user) "\"}")))

                 ;; `/r/<sub>/about.json` → subreddit entity; `/r/<sub>.json` → listing.
                 :else
                 (let [body (if (str/includes? path "/about") about listing)]
                   (send-bytes exchange 200 body "application/json"))))))]
     (.createContext server "/" handler)
     (.setExecutor server nil)
     (.start server)
     (fn stop []
       (.stop server 0)))))
