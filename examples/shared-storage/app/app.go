package main

import (
	"fmt"
	"io/ioutil"
	"log"
	"net/http"
	"os"
	"strconv"
)

func getCounter() int {
    data, err := ioutil.ReadFile("/data/counter.txt")
    if data == nil {
	    log.Printf("error: %s", err)
		data = []byte("0")
    }
	counter, _ := strconv.Atoi(string(data))
	counter++
	counters := []byte(strconv.Itoa(counter))
	ioutil.WriteFile("/data/counter.txt", counters, 0644)
	return counter
}

func handler(w http.ResponseWriter, r *http.Request) {
	hostname := os.Getenv("HOSTNAME")
	counter := getCounter()
	fmt.Fprintf(w, "running on %s, counter = %d", hostname, counter)
}

func main() {
	http.HandleFunc("/", handler)
	log.Fatal(http.ListenAndServe(":8080", nil))
}