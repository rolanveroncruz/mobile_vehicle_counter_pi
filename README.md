# Mobile Vehicle Counter Server


This is the Autonomous Systems Inc.'s project to count vehicles, and analyze traffic through a subdivision gate.

This specific project is the Server. Currently, this server is a single server that accepts POSTED data from raspberry pi 5 that are deployed. It also serves data to an angular frontend to report data to analysts.

However, we should separate this single server into two servers: 
1. one listens to events from the deployed raspberry pi 5 units,and writes these to the sqlite database.
2. the other reads from the database and publishes the data to the angular frontend.

We separate these servers so that we can keep revising the server to the frontend which we expect to need changes, while keeping the data receiving server constantly running.
