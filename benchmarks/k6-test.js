import http from "k6/http";
import ws from "k6/ws";
import { check, sleep } from "k6";

// Configuration variables
const emitApiKey = "reptar";
const subscribeApiKey = "szczuropies";
const baseUrl = "http://localhost:3030/emit";
const wsUrl = "ws://localhost:3030/subscribe";

export const options = {
  stages: [
    { duration: "5s", target: 500 },
    { duration: "2s", target: 500 },
    { duration: "3s", target: 1000 },
    { duration: "2s", target: 1000 },
    { duration: "3s", target: 0 },
    { duration: "2s", target: 0 },
    { duration: "3s", target: 1000 },
    { duration: "5s", target: 1000 },
    { duration: "5s", target: 1500 },
    { duration: "5s", target: 1500 },
    { duration: "2s", target: 500 },
    { duration: "3s", target: 500 },
    { duration: "1s", target: 0 },
  ],
  thresholds: {
    http_req_duration: ["p(95)<500"], // 95% of requests must complete below 500ms
    ws_connecting: ["p(95)<1000"], // 95% of WebSocket connections must be established in under 1s
  },
};

// HTTP emit endpoint test
export function httpEmit() {
  const payload = JSON.stringify({
    message: "This is a test message from k6",
  });

  const params = {
    headers: {
      "Content-Type": "application/json",
      Authorization: `Bearer ${emitApiKey}`,
    },
  };

  const response = http.post(`${baseUrl}/emit`, payload, params);
  check(response, {
    "emit status is 200": (r) => r.status === 200,
    "response has message": (r) => JSON.parse(r.body).message != null,
  });

  sleep(1);
}

// WebSocket subscriber test
export function wsSubscriber() {
  const url = `${wsUrl}/subscribe?token=${subscribeApiKey}`;
  let messageReceived = false;

  const res = ws.connect(url, {}, function (socket) {
    socket.on("open", () => {
      console.log("WebSocket connected");
    });

    socket.on("message", (msg) => {
      messageReceived = true;
      console.log(`Message received: ${msg}`);
    });

    socket.on("close", () => console.log("WebSocket disconnected"));

    socket.on("error", (e) => console.log("WebSocket error: ", e));

    // Keep connection open for 30 seconds
    socket.setTimeout(function () {
      socket.close();
    }, 30000);
  });

  check(res, {
    "WebSocket connection established": (r) => r && r.status === 101,
  });

  sleep(30);
}

// Mixed scenario
export default function () {
  if (Math.random() < 0.2) {
    // 20% of traffic is HTTP POST
    httpEmit();
  } else {
    // 80% of traffic is WebSocket connections
    wsSubscriber();
  }
}
