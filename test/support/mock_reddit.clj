(ns test.support.mock-reddit
  "In-process HTTP stub for Reddit API fixtures (`test/fixtures/reddit/`)."
  (:require [clojure.java.io :as io]
            [clojure.string :as str])
  (:import [com.sun.net.httpserver HttpServer HttpHandler HttpExchange]
           [java.net InetSocketAddress]))

(defn fixtures-dir
  ([] (fixtures-dir (System/getProperty "user.dir")))
  ([root] (str root "/test/fixtures/reddit")))

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
             ;; `/r/<sub>/about.json` → subreddit entity; `/r/<sub>.json` → listing.
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
       (.stop server 0)))))
