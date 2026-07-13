(ns test.support.mock-oauth
  "In-process HTTP stub for GitHub OAuth (authorize, token, /user)."
  (:require [clojure.string :as str])
  (:import [com.sun.net.httpserver HttpServer HttpHandler HttpExchange]
           [java.net InetSocketAddress URLDecoder]))

(defn- query-param [query key]
  (when query
    (some (fn [pair]
            (let [[k v] (str/split pair "=" 2)]
              (when (= k key)
                (URLDecoder/decode (or v "") "UTF-8"))))
          (str/split query #"&"))))

(defn- parse-mock-user [raw]
  (let [s (or raw "1002:newbie")
        [id login] (str/split s #":" 2)]
    {:id (Long/parseLong id)
     :login (or login "newbie")}))

(defn- send-json [^HttpExchange ex status body]
  (let [bytes (.getBytes body "UTF-8")]
    (.set (.getResponseHeaders ex) "Content-Type" "application/json")
    (.sendResponseHeaders ex status (alength bytes))
    (doto (.getResponseBody ex)
      (.write bytes)
      (.close))))

(defn- send-redirect [^HttpExchange ex location]
  (.set (.getResponseHeaders ex) "Location" location)
  (.sendResponseHeaders ex 302 -1)
  (.close (.getResponseBody ex)))

(defn- read-form-code [^HttpExchange ex]
  (let [body (slurp (.getInputStream ex))]
    (query-param body "code")))

(defn- bearer-token [^HttpExchange ex]
  (some-> (.getRequestHeaders ex)
          (.getFirst "Authorization")
          (str/replace #"^[Bb]earer " "")))

(defn- parse-token-user [token]
  (when (str/starts-with? token "mock:")
    (parse-mock-user (subs token 5))))

(defn start-mock-oauth
  "Start mock GitHub OAuth on `port`. Returns a zero-arg `stop` function."
  [port]
  (let [server (HttpServer/create (InetSocketAddress. "127.0.0.1" port) 0)
        handler
        (proxy [HttpHandler] []
          (handle [^HttpExchange exchange]
            (let [uri (.getRequestURI exchange)
                  path (.getPath uri)
                  query (.getQuery uri)]
              (cond
                (str/ends-with? path "/login/oauth/authorize")
                (let [redirect-uri (query-param query "redirect_uri")
                      state (query-param query "state")
                      mock-user (query-param query "mock_user")
                      user (parse-mock-user mock-user)
                      code (str "mock:" (:id user) ":" (:login user))
                      loc (str redirect-uri "?code=" (java.net.URLEncoder/encode code "UTF-8")
                               "&state=" (java.net.URLEncoder/encode state "UTF-8"))]
                  (send-redirect exchange loc))

                (str/ends-with? path "/login/oauth/access_token")
                (let [code (or (read-form-code exchange) "mock:1002:newbie")]
                  (send-json exchange 200 (str "{\"access_token\":\"" code "\",\"token_type\":\"bearer\"}")))

                (= path "/user")
                (let [token (bearer-token exchange)
                      user (or (parse-token-user token) {:id 1002 :login "newbie"})]
                  (send-json exchange 200
                             (str "{\"id\":" (:id user) ",\"login\":\"" (:login user) "\"}")))

                :else
                (send-json exchange 404 "{\"error\":\"not found\"}")))))]
    (.createContext server "/" handler)
    (.setExecutor server nil)
    (.start server)
    (fn stop []
      (.stop server 0))))
