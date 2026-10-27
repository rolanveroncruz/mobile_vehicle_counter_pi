# Mobile Vehicle Counter Server


This is the Autonomous Systems Inc.'s project to count vehicles, and analyze traffic through a subdivision gate.

This specific project is the Server. Currently, this server is a single server that accepts POSTED data from raspberry pi 5 that are deployed. It also serves data to an angular frontend to report data to analysts.

However, we should separate this single server into two servers: 
1. one listens to events from the deployed raspberry pi 5 units,and writes these to the sqlite database.
2. the other reads from the database and publishes the data to the angular frontend.

We separate these servers so that we can keep revising the server to the frontend which we expect to need changes, while keeping the data receiving server constantly running.


## Two Servers
This project creates two servers:

1. data_collector -calls collector::run_server() which sets up an http server whose only task is to receive POSTED base64 data, process it, make data insertions into the database, and save some files as image files.

2. frontend_api -calls api_server::run_server() which sets up an http server whose task is to be the backend server for a frontend angular app.


## Data Collector
The data collector accepts POST-ed data from deployed raspberry pis which just relay the data from cameras. The data structure they accept should be identical to what a raspberry pi collects.
