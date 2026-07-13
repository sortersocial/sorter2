(ns test.support.seed-auth
  "Append-only event-log seeds for auth integration tests.")

(def seeder-uuid "00000000-0000-0000-0000-000000000010")

(def rust-scope "https://reddit.com/r/rust")
(def post-a "https://reddit.com/r/rust/comments/aaa")
(def post-b "https://reddit.com/r/rust/comments/bbb")

(defn- esc [s]
  (.replace s "\\" "\\\\"))

(defn- line [seq ts event-json]
  (str "{\"schema\":2,\"seq\":" seq ",\"ts\":" ts ",\"event\":" event-json "}" "\n"))

(defn seeder-vote-events
  "Events that register a seeder principal and one vote on the rust A/B pair."
  []
  [(line 1 1 (str "{\"type\":\"principal_created\",\"uuid\":\"" (esc seeder-uuid) "\",\"ts\":1}"))
   (line 2 2 (str "{\"type\":\"oauth_linked\",\"uuid\":\"" (esc seeder-uuid)
                   "\",\"provider\":\"github\",\"provider_id\":\"1001\",\"ts\":2}"))
   (line 3 3 (str "{\"type\":\"pseudonym_claimed\",\"uuid\":\"" (esc seeder-uuid)
                   "\",\"pseudonym\":\"seeder\",\"ts\":3}"))
   (line 4 4 (str "{\"type\":\"node_ensured\",\"id\":\"" (esc rust-scope) "\"}"))
   (line 5 5 (str "{\"type\":\"node_ensured\",\"id\":\"" (esc post-a) "\"}"))
   (line 6 6 (str "{\"type\":\"node_ensured\",\"id\":\"" (esc post-b) "\"}"))
   (line 7 7 (str "{\"type\":\"vote_recorded\",\"ts\":7"
                   ",\"a\":\"" (esc post-a) "\",\"b\":\"" (esc post-b) "\""
                   ",\"ratio_left\":3,\"ratio_right\":1"
                   ",\"scope\":\"" (esc rust-scope) "\""
                   ",\"pseudonym\":\"seeder\",\"trust_weight\":1.5}"))])

(defn write-seeder-events!
  [event-log-path]
  (spit event-log-path (apply str (seeder-vote-events))))

(defn seeder-pair-vote-url [app-base]
  (str app-base "/vote?parent="
       (java.net.URLEncoder/encode rust-scope "UTF-8")
       "&left=" (java.net.URLEncoder/encode post-a "UTF-8")
       "&right=" (java.net.URLEncoder/encode post-b "UTF-8")))
