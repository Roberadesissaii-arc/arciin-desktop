import React from "react"
import ReactDOM from "react-dom/client"

import { App } from "@/app/App"

import "@/styles/theme.css"
import "@/styles/shell.css"
import "@/styles/components.css"

const root = document.getElementById("root")
if (!root) throw new Error("#root is missing from index.html")

ReactDOM.createRoot(root).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
)
